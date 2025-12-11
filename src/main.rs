use std::{
	collections::{HashMap, VecDeque},
	env,
	io::{Read, Write},
	net::{TcpListener, TcpStream},
	sync::{
		atomic::{AtomicU32, Ordering},
		mpsc,
		Arc,
	},
	thread,
	time::{Duration, Instant},
};

use macroquad::{miniquad::window::set_window_size, prelude::*};

const BUFFER_W: u32 = 240;
const BUFFER_H: u32 = 140;
const TPS: u32 = 50;
const DT: f32 = 1.0 / TPS as f32;

const PLAYER_COUNT: usize = 2;

// The server intentionally runs this many ticks behind wall-clock
// Larger values allow higher one-way latency before inputs become "too late"
const LEAD_TICKS: u32 = 4;

// Server accepts inputs up to this far in the future
// Must be >= LEAD_TICKS
const D_MAX: u32 = 32;

// How much tick history to keep for rollback
const HISTORY: usize = 2048;

// Max number of ticks we will simulate in one rendered frame to catch up when behind
const CATCHUP_BUDGET_TICKS: u32 = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
	Server,
	Client,
	Malicious,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct InputBits(u8);

impl InputBits {
	const LEFT: u8 = 1 << 0;
	const RIGHT: u8 = 1 << 1;
	const JUMP: u8 = 1 << 2;

	fn from_keyboard() -> Self {
		let mut b = 0u8;
		if is_key_down(KeyCode::A) {
			b |= Self::LEFT;
		}
		if is_key_down(KeyCode::D) {
			b |= Self::RIGHT;
		}
		if is_key_down(KeyCode::Space) {
			b |= Self::JUMP;
		}
		Self(b)
	}

	fn left(self) -> bool {
		(self.0 & Self::LEFT) != 0
	}

	fn right(self) -> bool {
		(self.0 & Self::RIGHT) != 0
	}
	
	fn jump(self) -> bool {
		(self.0 & Self::JUMP) != 0
	}
}

#[derive(Clone, Copy)]
pub struct Player {
	x: f32,
	y: f32,
	vx: f32,
	vy: f32,
}

impl Player {
	pub const WIDTH: f32 = 32.0;
	pub const HEIGHT: f32 = 32.0;

	pub fn new() -> Self {
		Self {
			x: 10.0,
			y: 10.0,
			vx: 0.0,
			vy: 0.0,
		}
	}
}

#[derive(Clone, Copy)]
struct SimState {
	players: [Player; PLAYER_COUNT],
}

impl SimState {
	fn new() -> Self {
		let mut p0 = Player::new();
		let mut p1 = Player::new();
		p0.x = 20.0;
		p0.y = 20.0;
		p1.x = 100.0;
		p1.y = 20.0;
		Self { players: [p0, p1] }
	}
}

fn step(state: &mut SimState, inputs: [InputBits; PLAYER_COUNT]) {
	const GRAVITY: f32 = 600.0;
	const MOVE_SPEED: f32 = 90.0;
	const JUMP_SPEED: f32 = 220.0;

	for (i, player) in state.players.iter_mut().enumerate() {
		let input = inputs[i];

		let mut dx = 0i32;
		if input.left() {
			dx -= 1;
		}
		if input.right() {
			dx += 1;
		}
		player.vx = dx as f32 * MOVE_SPEED;

		let on_ground = player.y + Player::HEIGHT >= BUFFER_H as f32;
		if input.jump() && on_ground {
			player.vy = -JUMP_SPEED;
		}

		// gravity
		player.vy += GRAVITY * DT;

		// integrate
		player.x += player.vx * DT;
		player.y += player.vy * DT;

		// collide / clamp to buffer bounds
		if player.x < 0.0 {
			player.x = 0.0;
		}
		let max_x = BUFFER_W as f32 - Player::WIDTH;
		if player.x > max_x {
			player.x = max_x;
		}

		if player.y < 0.0 {
			player.y = 0.0;
			if player.vy < 0.0 {
				player.vy = 0.0;
			}
		}
		let max_y = BUFFER_H as f32 - Player::HEIGHT;
		if player.y > max_y {
			player.y = max_y;
			if player.vy > 0.0 {
				player.vy = 0.0;
			}
		}
	}
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
	a + (b - a) * t
}

const TAG_ASSIGN_START: u8 = 1;
const TAG_INPUT: u8 = 2;
const TAG_TICK_INPUTS: u8 = 3;

#[derive(Debug, Clone, Copy)]
struct AssignStart {
	player_id: u8,
	start_after_ms: u32,
}

#[derive(Debug, Clone, Copy)]
struct InputMsg {
	player_id: u8,
	tick: u32,
	bits: InputBits,
}

#[derive(Debug, Clone, Copy)]
struct TickInputsMsg {
	tick: u32,
	inputs: [InputBits; PLAYER_COUNT],
}

fn write_u32_le(w: &mut impl Write, v: u32) -> std::io::Result<()> {
	w.write_all(&v.to_le_bytes())
}

fn read_u32_le(r: &mut impl Read) -> std::io::Result<u32> {
	let mut b = [0u8; 4];
	r.read_exact(&mut b)?;
	Ok(u32::from_le_bytes(b))
}

fn send_assign_start(stream: &mut TcpStream, msg: AssignStart) -> std::io::Result<()> {
	stream.write_all(&[TAG_ASSIGN_START, msg.player_id])?;
	write_u32_le(stream, msg.start_after_ms)?;
	Ok(())
}

fn send_input(stream: &mut TcpStream, msg: InputMsg) -> std::io::Result<()> {
	stream.write_all(&[TAG_INPUT, msg.player_id])?;
	write_u32_le(stream, msg.tick)?;
	stream.write_all(&[msg.bits.0])?;
	Ok(())
}

fn send_tick_inputs(stream: &mut TcpStream, msg: TickInputsMsg) -> std::io::Result<()> {
	stream.write_all(&[TAG_TICK_INPUTS])?;
	write_u32_le(stream, msg.tick)?;
	stream.write_all(&[msg.inputs[0].0, msg.inputs[1].0])?;
	Ok(())
}

fn recv_msg(stream: &mut TcpStream) -> std::io::Result<Option<NetEvent>> {
	let mut tag = [0u8; 1];
	match stream.read_exact(&mut tag) {
		Ok(()) => {}
		Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
		Err(e) => return Err(e),
	}

	match tag[0] {
		TAG_ASSIGN_START => {
			let mut idb = [0u8; 1];
			stream.read_exact(&mut idb)?;
			let start_after_ms = read_u32_le(stream)?;
			Ok(Some(NetEvent::AssignStart(AssignStart {
				player_id: idb[0],
				start_after_ms,
			})))
		}
		TAG_TICK_INPUTS => {
			let tick = read_u32_le(stream)?;
			let mut bits = [0u8; 2];
			stream.read_exact(&mut bits)?;
			Ok(Some(NetEvent::TickInputs(TickInputsMsg {
				tick,
				inputs: [InputBits(bits[0]), InputBits(bits[1])],
			})))
		}
		TAG_INPUT => {
			let mut pid = [0u8; 1];
			stream.read_exact(&mut pid)?;
			let _tick = read_u32_le(stream)?;
			let mut b = [0u8; 1];
			stream.read_exact(&mut b)?;
			Ok(None)
		}
		other => Err(std::io::Error::new(
			std::io::ErrorKind::InvalidData,
			format!("unknown tag {other}"),
		)),
	}
}

enum NetEvent {
	AssignStart(AssignStart),
	TickInputs(TickInputsMsg),
}

enum NetCmd {
	SetMyId(u8),
	SendInput { tick: u32, bits: InputBits },
}

fn parse_runtime_and_addr() -> (Runtime, String) {
	let mut runtime = Runtime::Client;
	let mut addr = "127.0.0.1:4000".to_string();

	let mut it = env::args().skip(1);
	while let Some(a) = it.next() {
		match a.as_str() {
			"--runtime" => {
				if let Some(v) = it.next() {
					runtime = match v.as_str() {
						"server" => Runtime::Server,
						"client" => Runtime::Client,
						"malicious" => Runtime::Malicious,
						_ => Runtime::Client,
					}
				}
			}
			"--addr" => {
				if let Some(v) = it.next() {
					addr = v;
				}
			}
			_ => {}
		}
	}

	(runtime, addr)
}

struct ServerRender {
	tick: u32,
	state: SimState,
}

struct ServerConn {
	stream: TcpStream,
}

fn spawn_server(addr: String) -> mpsc::Receiver<ServerRender> {
	let (tx_render, rx_render) = mpsc::channel::<ServerRender>();

	thread::spawn(move || {
		let listener = TcpListener::bind(&addr).expect("bind server");
		listener
			.set_nonblocking(false)
			.expect("set_nonblocking(false)");

		let mut conns: Vec<ServerConn> = Vec::new();
		let (tx_in, rx_in) = mpsc::channel::<InputMsg>();

		for assigned_id in 0u8..2u8 {
			let (stream, _) = listener.accept().expect("accept");
			stream.set_nodelay(true).ok();
			let mut read_stream = stream.try_clone().expect("clone stream");
			read_stream.set_nodelay(true).ok();

			let tx_in = tx_in.clone();
			let assigned_id_for_thread = assigned_id;
			thread::spawn(move || loop {
				let mut tag = [0u8; 1];
				match read_stream.read_exact(&mut tag) {
					Ok(()) => {}
					Err(_) => break,
				}
				if tag[0] != TAG_INPUT {
					// ignore unknown direction
					break;
				}

				let mut pid = [0u8; 1];
				if read_stream.read_exact(&mut pid).is_err() {
					break;
				}
				let tick = match read_u32_le(&mut read_stream) {
					Ok(t) => t,
					Err(_) => break,
				};
				let mut b = [0u8; 1];
				if read_stream.read_exact(&mut b).is_err() {
					break;
				}
				let _ = tx_in.send(InputMsg {
					// Don't trust the client-provided player id
					player_id: assigned_id_for_thread,
					tick,
					bits: InputBits(b[0]),
				});
			});

			conns.push(ServerConn { stream });
		}

		// Choose a shared start instant and tell everyone
		let start_at = Instant::now() + Duration::from_millis(800);
		for (i, c) in conns.iter_mut().enumerate() {
			let now = Instant::now();
			let start_after_ms = start_at
				.saturating_duration_since(now)
				.as_millis()
				.min(u128::from(u32::MAX)) as u32;
			send_assign_start(
				&mut c.stream,
				AssignStart {
					player_id: i as u8,
					start_after_ms,
				},
			)
			.expect("send assign/start");
		}
		while Instant::now() < start_at {
			thread::sleep(Duration::from_millis(5));
		}

		let mut tick: u32 = 0;
		let mut state = SimState::new();

		// Pending future inputs by tick per player.
		// The server respects the tick included in the message, arrival order doesn't matter.
		let mut pending: [HashMap<u32, InputBits>; PLAYER_COUNT] =
			[HashMap::new(), HashMap::new()];
		let mut last: [InputBits; PLAYER_COUNT] = [InputBits(0), InputBits(0)];

		let mut last_step = Instant::now();
		let mut acc = 0.0f32;

		loop {
			let now = Instant::now();
			let dt = now.duration_since(last_step);
			last_step = now;
			acc += dt.as_secs_f32();

			// Keep server tick intentionally behind wall-clock
			let elapsed = now.saturating_duration_since(start_at);
			let wall_tick = (elapsed.as_secs_f32() * TPS as f32).floor() as u32;
			let max_tick = wall_tick.saturating_sub(LEAD_TICKS);

			// Drain inbound inputs
			while let Ok(msg) = rx_in.try_recv() {
				let pid = msg.player_id as usize;
				if pid >= PLAYER_COUNT {
					continue;
				}
				// Enforce "no retroactive inputs" and bounded future window
				if msg.tick < tick {
					continue;
				}
				if msg.tick > tick.saturating_add(D_MAX) {
					continue;
				}
				// Once a tick is submitted for this player, ignore later changes
				pending[pid].entry(msg.tick).or_insert(msg.bits);
			}

			while acc >= DT && tick <= max_tick {
				// Build authoritative inputs for this tick
				let mut inputs = last;
				for pid in 0..PLAYER_COUNT {
					if let Some(b) = pending[pid].remove(&tick) {
						inputs[pid] = b;
						last[pid] = b;
					}
				}

				// Broadcast authoritative inputs for tick
				let msg = TickInputsMsg { tick, inputs };
				conns.retain_mut(|c| send_tick_inputs(&mut c.stream, msg).is_ok());

				// Simulate on server too (visualization)
				step(&mut state, inputs);

				let _ = tx_render.send(ServerRender { tick, state });

				tick = tick.wrapping_add(1);
				acc -= DT;
			}

			thread::sleep(Duration::from_millis(1));
		}
	});

	rx_render
}

fn spawn_client_net(
	addr: String,
) -> (mpsc::Receiver<NetEvent>, mpsc::Sender<NetCmd>) {
	let (tx_evt, rx_evt) = mpsc::channel::<NetEvent>();
	let (tx_cmd, rx_cmd) = mpsc::channel::<NetCmd>();

	// Connect and split into reader/writer threads
	let stream = TcpStream::connect(&addr).expect("connect");
	stream.set_nodelay(true).ok();

	let mut read_stream = stream.try_clone().expect("clone read stream");
	let mut write_stream = stream;

	// Reader
	let tx_evt_r = tx_evt.clone();
	thread::spawn(move || loop {
		match recv_msg(&mut read_stream) {
			Ok(Some(ev)) => {
				if tx_evt_r.send(ev).is_err() {
					break;
				}
			}
			Ok(None) => {}
			Err(_) => break,
		}
	});

	// Writer
	thread::spawn(move || {
		let mut my_id: u8 = 0;
		while let Ok(cmd) = rx_cmd.recv() {
			match cmd {
				NetCmd::SetMyId(id) => {
					my_id = id;
				}
				NetCmd::SendInput { tick, bits } => {
					let _ = send_input(
						&mut write_stream,
						InputMsg {
							player_id: my_id,
							tick,
							bits,
						},
					);
				}
			}
		}
	});

	(rx_evt, tx_cmd)
}

#[macroquad::main("Demo")]
async fn main() {
	let (runtime, addr) = parse_runtime_and_addr();

	let buffer = render_target(BUFFER_W, BUFFER_H);
	buffer.texture.set_filter(FilterMode::Nearest);

	set_window_size(BUFFER_W * 3, BUFFER_H * 3);

	// Server runtime
	if matches!(runtime, Runtime::Server) {
		let rx_render = spawn_server(addr);
		let mut latest = ServerRender {
			tick: 0,
			state: SimState::new(),
		};

		loop {
			while let Ok(r) = rx_render.try_recv() {
				latest = r;
			}

			set_camera(&Camera2D {
				render_target: Some(buffer.clone()),
				zoom: vec2(2.0 / BUFFER_W as f32, 2.0 / BUFFER_H as f32),
				target: vec2(BUFFER_W as f32 / 2.0, BUFFER_H as f32 / 2.0),
				..Default::default()
			});
			clear_background(BLACK);

			for (i, p) in latest.state.players.iter().enumerate() {
				let color = if i == 0 { BLUE } else { RED };
				draw_rectangle(p.x, p.y, Player::WIDTH, Player::HEIGHT, color);
			}

			set_default_camera();
			clear_background(BLACK);
			let sw = screen_width();
			let sh = screen_height();
			let scale = (sw / BUFFER_W as f32).min(sh / BUFFER_H as f32);
			let draw_w = BUFFER_W as f32 * scale;
			let draw_h = BUFFER_H as f32 * scale;
			let offset_x = ((sw - draw_w) / 2.0).round();
			let offset_y = ((sh - draw_h) / 2.0).round();
			draw_texture_ex(
				&buffer.texture,
				offset_x,
				offset_y,
				WHITE,
				DrawTextureParams {
					dest_size: Some(vec2(draw_w, draw_h)),
					..Default::default()
				},
			);

			draw_text(&format!("server tick {}", latest.tick), 10.0, 24.0, 24.0, WHITE);

			next_frame().await;
		}
	}

	// Client runtime
	let artificial_delay_ms = Arc::new(AtomicU32::new(0));
	let (rx_evt, tx_cmd) = spawn_client_net(addr);

	let mut my_id: usize = 0;
	let mut sim_start_at: Option<Instant> = None;
	let mut latest_server_tick: u32 = 0;

	// Authoritative inputs from server per tick
	let mut auth_inputs: Vec<Option<(u32, [InputBits; PLAYER_COUNT])>> = vec![None; HISTORY];

	// Inputs we actually used when simulating tick
	let mut used_inputs: Vec<Option<(u32, [InputBits; PLAYER_COUNT])>> = vec![None; HISTORY];
	
	// State at the start of tick
	let mut state_history: Vec<Option<(u32, SimState)>> = vec![None; HISTORY];

	let mut state = SimState::new();
	let mut render_prev_state = state;
	let mut local_tick: u32 = 0;
	let mut last_remote: [InputBits; PLAYER_COUNT] = [InputBits(0), InputBits(0)];
	let mut pending_rollback: Option<u32> = None;

	let mut accumulator: f32 = 0.0;

	// Delay queue for simulating inbound ping
	let mut delayed_events: VecDeque<(Instant, NetEvent)> = VecDeque::new();
	let mut last_scheduled_at: Option<Instant> = None;

	// Delay queue for simulating outbound ping
	let mut delayed_out_cmds: VecDeque<(Instant, NetCmd)> = VecDeque::new();
	let mut last_out_scheduled_at: Option<Instant> = None;

	loop {
		// Drain raw network events and schedule them for delivery.
		while let Ok(ev) = rx_evt.try_recv() {
			let now = Instant::now();
			let deliver_at = match ev {
				NetEvent::AssignStart(_) => now, // don't delay handshake
				NetEvent::TickInputs(_) => {
					let delay = artificial_delay_ms.load(Ordering::Relaxed);
					let desired = now + Duration::from_millis(delay as u64);
					let scheduled = match last_scheduled_at {
						Some(prev) if prev > desired => prev,
						_ => desired,
					};
					last_scheduled_at = Some(scheduled);
					scheduled
				}
			};
			delayed_events.push_back((deliver_at, ev));
		}

		// Deliver any events whose artificial delay has elapsed
		let now = Instant::now();
		while let Some((deliver_at, _)) = delayed_events.front() {
			if *deliver_at > now {
				break;
			}
			let (_, ev) = delayed_events.pop_front().unwrap();
			match ev {
				NetEvent::AssignStart(a) => {
					my_id = a.player_id as usize;
					let _ = tx_cmd.send(NetCmd::SetMyId(a.player_id));
					local_tick = 0;
					latest_server_tick = 0;
					sim_start_at =
						Some(Instant::now() + Duration::from_millis(a.start_after_ms as u64));

					// Reset sim/history
					state = SimState::new();
					render_prev_state = state;
					last_remote = [InputBits(0), InputBits(0)];
					pending_rollback = None;
					accumulator = 0.0;
					delayed_events.clear();
					last_scheduled_at = None;
					delayed_out_cmds.clear();
					last_out_scheduled_at = None;
					for slot in auth_inputs.iter_mut() {
						*slot = None;
					}
					for slot in used_inputs.iter_mut() {
						*slot = None;
					}
					for slot in state_history.iter_mut() {
						*slot = None;
					}
				}
				NetEvent::TickInputs(m) => {
					if m.tick > latest_server_tick {
						latest_server_tick = m.tick;
					}
					let idx = (m.tick as usize) % HISTORY;
					auth_inputs[idx] = Some((m.tick, m.inputs));

					// Track last known remote inputs for prediction
					last_remote[0] = m.inputs[0];
					last_remote[1] = m.inputs[1];

					// If we already simulated this tick, check mismatch and schedule rollback
					if let Some((t_used, used)) = used_inputs[idx] {
						if t_used == m.tick && used != m.inputs {
							pending_rollback = Some(match pending_rollback {
								Some(t0) => t0.min(m.tick),
								None => m.tick,
							});
						}
					}
				}
			};
		}

		// Flush any outbound commands whose artificial delay has elapsed
		let now = Instant::now();
		while let Some((send_at, _)) = delayed_out_cmds.front() {
			if *send_at > now {
				break;
			}
			let (_, cmd) = delayed_out_cmds.pop_front().unwrap();
			let _ = tx_cmd.send(cmd);
		}

		// Wait for start
		if let Some(start_at) = sim_start_at {
			if Instant::now() < start_at {
				set_camera(&Camera2D {
					render_target: Some(buffer.clone()),
					zoom: vec2(2.0 / BUFFER_W as f32, 2.0 / BUFFER_H as f32),
					target: vec2(BUFFER_W as f32 / 2.0, BUFFER_H as f32 / 2.0),
					..Default::default()
				});
				clear_background(BLACK);
				draw_text("waiting for start...", 4.0, 14.0, 16.0, WHITE);
				set_default_camera();
				clear_background(BLACK);
				let sw = screen_width();
				let sh = screen_height();
				let scale = (sw / BUFFER_W as f32).min(sh / BUFFER_H as f32);
				let draw_w = BUFFER_W as f32 * scale;
				let draw_h = BUFFER_H as f32 * scale;
				let offset_x = ((sw - draw_w) / 2.0).round();
				let offset_y = ((sh - draw_h) / 2.0).round();
				draw_texture_ex(
					&buffer.texture,
					offset_x,
					offset_y,
					WHITE,
					DrawTextureParams {
						dest_size: Some(vec2(draw_w, draw_h)),
						..Default::default()
					},
				);
				next_frame().await;
				continue;
			}
		} else {
			set_camera(&Camera2D {
				render_target: Some(buffer.clone()),
				zoom: vec2(2.0 / BUFFER_W as f32, 2.0 / BUFFER_H as f32),
				target: vec2(BUFFER_W as f32 / 2.0, BUFFER_H as f32 / 2.0),
				..Default::default()
			});
			clear_background(BLACK);
			draw_text("connecting...", 4.0, 14.0, 16.0, WHITE);
			set_default_camera();
			clear_background(BLACK);
			let sw = screen_width();
			let sh = screen_height();
			let scale = (sw / BUFFER_W as f32).min(sh / BUFFER_H as f32);
			let draw_w = BUFFER_W as f32 * scale;
			let draw_h = BUFFER_H as f32 * scale;
			let offset_x = ((sw - draw_w) / 2.0).round();
			let offset_y = ((sh - draw_h) / 2.0).round();
			draw_texture_ex(
				&buffer.texture,
				offset_x,
				offset_y,
				WHITE,
				DrawTextureParams {
					dest_size: Some(vec2(draw_w, draw_h)),
					..Default::default()
				},
			);
			next_frame().await;
			continue;
		}

		// Apply rollback if scheduled
		if let Some(t_rb) = pending_rollback {
			let idx = (t_rb as usize) % HISTORY;
			if let Some((t_saved, saved)) = state_history[idx] {
				if t_saved == t_rb {
					state = saved;
					render_prev_state = state;
					local_tick = t_rb;
					pending_rollback = None;
				}
			}
		}

		if is_key_pressed(KeyCode::Left) {
			let cur = artificial_delay_ms.load(Ordering::Relaxed);
			artificial_delay_ms.store(cur.saturating_sub(10), Ordering::Relaxed);
		}

		if is_key_pressed(KeyCode::Right) {
			let cur = artificial_delay_ms.load(Ordering::Relaxed);
			artificial_delay_ms.store(cur.saturating_add(10), Ordering::Relaxed);
		}

		// Target tick is driven by local time since the shared start instant
		// This prevents a client that is receiving packets late from falling behind the server tick
		let time_tick = sim_start_at
			.map(|start_at| {
				let elapsed = Instant::now().saturating_duration_since(start_at);
				(elapsed.as_secs_f32() * TPS as f32).floor() as u32
			})
			.unwrap_or(0);

		// Client runs at wall-clock tick
		let target_tick = time_tick;

		// Overwatch style time dilation: adjust based on where we should be relative to the server
		let expected_server_tick = time_tick.saturating_sub(LEAD_TICKS);
		let ahead = local_tick as i64 - expected_server_tick as i64;
		let sim_rate = if ahead < (LEAD_TICKS as i64 - 1) {
			1.03
		} else if ahead > (LEAD_TICKS as i64 + 2) {
			0.98
		} else {
			1.0
		};
		accumulator += get_frame_time() * sim_rate;

		// If we're behind target_tick, simulate in a burst to catch up, even if accumulator hasn't
		// earned that many ticks yet.
		let mut steps_this_frame: u32 = 0;
		while local_tick < target_tick
			&& steps_this_frame < CATCHUP_BUDGET_TICKS
			&& (accumulator >= DT || local_tick + 1 < target_tick)
		{
			if accumulator < DT {
				// Force a tick of simulation to burn backlog (catch-up).
				accumulator = DT;
			}

			// Save previous state for render interpolation
			render_prev_state = state;

			// Choose inputs for this tick
			let idx = (local_tick as usize) % HISTORY;

			// Save state at start of tick
			state_history[idx] = Some((local_tick, state));

			let mut inputs: [InputBits; PLAYER_COUNT] = [InputBits(0), InputBits(0)];

			// If we already have authoritative inputs for this tick, use them
			let mut have_auth = false;
			if let Some((t, auth)) = auth_inputs[idx] {
				if t == local_tick {
					inputs = auth;
					have_auth = true;
				}
			}

			// Otherwise, predict missing
			if !have_auth {
				for pid in 0..PLAYER_COUNT {
					if pid == my_id {
						inputs[pid] = InputBits::from_keyboard();
					} else {
						inputs[pid] = last_remote[pid];
					}
				}
			}

			// Record what we used
			used_inputs[idx] = Some((local_tick, inputs));

			// Send our input for this tick to the server
			let delay = artificial_delay_ms.load(Ordering::Relaxed);
			let now = Instant::now();
			let desired = now + Duration::from_millis(delay as u64);
			let scheduled = match last_out_scheduled_at {
				Some(prev) if prev > desired => prev,
				_ => desired,
			};
			last_out_scheduled_at = Some(scheduled);
			delayed_out_cmds.push_back((
				scheduled,
				NetCmd::SendInput {
					tick: local_tick,
					bits: inputs[my_id],
				},
			));

			// Simulate
			step(&mut state, inputs);

			// Malicious mode: locally tamper with our position after sim
			if matches!(runtime, Runtime::Malicious) {
				state.players[my_id].y -= 20.0;
				state.players[my_id].vy = 0.0;
			}

			local_tick = local_tick.wrapping_add(1);
			accumulator -= DT;
			steps_this_frame += 1;
		}

		// Interpolate between the last two simulated states for smooth rendering
		let alpha = (accumulator / DT).clamp(0.0, 1.0);

		set_camera(&Camera2D {
			render_target: Some(buffer.clone()),
			zoom: vec2(2.0 / BUFFER_W as f32, 2.0 / BUFFER_H as f32),
			target: vec2(BUFFER_W as f32 / 2.0, BUFFER_H as f32 / 2.0),
			..Default::default()
		});
		
		clear_background(BLACK);

		let title = match runtime {
			Runtime::Client => "client",
			Runtime::Malicious => "malicious",
			Runtime::Server => "server",
		};
		let debug_text = format!(
			"{title} me={} tick={} srv={} delay={}ms (←/→ adjust)",
			my_id,
			local_tick,
			latest_server_tick,
			artificial_delay_ms.load(Ordering::Relaxed)
		);

		for (i, p) in state.players.iter().enumerate() {
			let color = if i == 0 { BLUE } else { RED };
			let prev = render_prev_state.players[i];
			let x = lerp(prev.x, p.x, alpha);
			let y = lerp(prev.y, p.y, alpha);
			draw_rectangle(x, y, Player::WIDTH, Player::HEIGHT, color);
		}

		set_default_camera();

		clear_background(BLACK);

		let sw = screen_width();
		let sh = screen_height();

		let scale_x = sw / BUFFER_W as f32;
		let scale_y = sh / BUFFER_H as f32;
		let scale = scale_x.min(scale_y);

		let draw_w = BUFFER_W as f32 * scale;
		let draw_h = BUFFER_H as f32 * scale;

		// snap to integer pixels to avoid sampling half-pixels
		let offset_x = ((sw - draw_w) / 2.0).round();
		let offset_y = ((sh - draw_h) / 2.0).round();

		draw_texture_ex(
			&buffer.texture,
			offset_x,
			offset_y,
			WHITE,
			DrawTextureParams {
				dest_size: Some(vec2(draw_w, draw_h)),
				..Default::default()
			},
		);

		draw_text(&debug_text, 10.0, 24.0, 24.0, WHITE);
		draw_text(
			&format!("lead={} d_max={}", LEAD_TICKS, D_MAX),
			10.0,
			46.0,
			22.0,
			WHITE,
		);

		next_frame().await
	}
}