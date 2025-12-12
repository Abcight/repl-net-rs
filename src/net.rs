use std::{
	io::{Read, Write},
	net::{TcpListener, TcpStream},
	sync::mpsc,
	thread,
	time::{Duration, Instant},
};

use anyhow::Context;

use crate::protocol::{AssignStart, C2S, S2C, TickInputs, PLAYER_COUNT};

fn write_frame(stream: &mut TcpStream, msg: &impl serde::Serialize) -> anyhow::Result<()> {
	let bytes = bincode::serialize(msg)?;
	let len = bytes.len() as u32;
	stream.write_all(&len.to_le_bytes())?;
	stream.write_all(&bytes)?;
	Ok(())
}

fn read_frame<T: for<'de> serde::Deserialize<'de>>(stream: &mut TcpStream) -> anyhow::Result<T> {
	let mut lenb = [0u8; 4];
	stream.read_exact(&mut lenb)?;
	let len = u32::from_le_bytes(lenb) as usize;
	let mut buf = vec![0u8; len];
	stream.read_exact(&mut buf)?;
	Ok(bincode::deserialize(&buf)?)
}

#[derive(Debug, Clone, Copy)]
pub struct ServerRender {
	pub tick: u32,
	pub state: crate::sim::SimState,
}

#[derive(Debug, Clone, Copy)]
pub struct InboundInput {
	pub player_id: usize,
	pub tick: u32,
	pub bits: u8,
}

pub fn spawn_server(
	addr: String,
	start_delay: Duration,
	lead_ticks: u32,
	d_max: u32,
) -> mpsc::Receiver<ServerRender> {
	let (tx_render, rx_render) = mpsc::channel::<ServerRender>();

	thread::spawn(move || {
		let listener = TcpListener::bind(&addr).expect("bind server");

		let mut conns: Vec<TcpStream> = Vec::new();
		let (tx_in, rx_in) = mpsc::channel::<InboundInput>();

		for assigned_id in 0u8..2u8 {
			let (stream, _) = listener.accept().expect("accept");
			stream.set_nodelay(true).ok();
			let mut read_stream = stream.try_clone().expect("clone stream");

			let tx_in = tx_in.clone();
			let pid = assigned_id as usize;
			thread::spawn(move || loop {
				let msg: anyhow::Result<C2S> = read_frame(&mut read_stream);
				let Ok(C2S::Input(i)) = msg else { break };
				let _ = tx_in.send(InboundInput {
					player_id: pid, // don't trust client
					tick: i.tick,
					bits: i.bits,
				});
			});

			conns.push(stream);
		}

		// Shared start instant, then notify everyone
		let start_at = Instant::now() + start_delay;
		for (i, s) in conns.iter_mut().enumerate() {
			let start_after_ms = start_at
				.saturating_duration_since(Instant::now())
				.as_millis()
				.min(u128::from(u32::MAX)) as u32;
			let msg = S2C::AssignStart(AssignStart {
				player_id: i as u8,
				start_after_ms,
			});
			let _ = write_frame(s, &msg);
		}
		while Instant::now() < start_at {
			thread::sleep(Duration::from_millis(5));
		}

		let mut tick: u32 = 0;
		let mut state = crate::sim::SimState::new();

		let mut pending: [std::collections::HashMap<u32, u8>; PLAYER_COUNT] =
			[Default::default(), Default::default()];
		let mut last: [u8; PLAYER_COUNT] = [0, 0];

		let mut last_step = Instant::now();
		let mut acc = 0.0f32;

		loop {
			let now = Instant::now();
			acc += now.duration_since(last_step).as_secs_f32();
			last_step = now;

			// Keep server tick behind clock by lead_ticks
			let elapsed = now.saturating_duration_since(start_at);
			let wall_tick = (elapsed.as_secs_f32() * crate::sim::TPS as f32).floor() as u32;
			let max_tick = wall_tick.saturating_sub(lead_ticks);

			while let Ok(msg) = rx_in.try_recv() {
				let pid = msg.player_id;
				if msg.tick < tick {
					continue;
				}
				if msg.tick > tick.saturating_add(d_max) {
					continue;
				}
				pending[pid].entry(msg.tick).or_insert(msg.bits);
			}

			while acc >= crate::sim::DT && tick <= max_tick {
				let mut inputs = last;
				for pid in 0..PLAYER_COUNT {
					if let Some(b) = pending[pid].remove(&tick) {
						inputs[pid] = b;
						last[pid] = b;
					}
				}

				let s2c = S2C::TickInputs(TickInputs { tick, inputs });
				conns.retain_mut(|s| write_frame(s, &s2c).is_ok());

				let sim_inputs = [
					crate::sim::InputBits::from_u8(inputs[0]),
					crate::sim::InputBits::from_u8(inputs[1]),
				];
				crate::sim::step(&mut state, sim_inputs);

				let _ = tx_render.send(ServerRender { tick, state });

				tick = tick.wrapping_add(1);
				acc -= crate::sim::DT;
			}

			thread::sleep(Duration::from_millis(1));
		}
	});

	rx_render
}

pub enum NetEvent {
	AssignStart(AssignStart),
	TickInputs(TickInputs),
}

#[derive(Debug, Clone, Copy)]
pub enum NetCmd {
	SendInput { tick: u32, bits: u8 },
}

pub fn spawn_client(addr: String) -> anyhow::Result<(mpsc::Receiver<NetEvent>, mpsc::Sender<NetCmd>)> {
	let (tx_evt, rx_evt) = mpsc::channel::<NetEvent>();
	let (tx_cmd, rx_cmd) = mpsc::channel::<NetCmd>();

	let stream = TcpStream::connect(&addr).context("connect")?;
	stream.set_nodelay(true).ok();
	let mut read_stream = stream.try_clone().context("clone read stream")?;
	let mut write_stream = stream;

	// Reader
	thread::spawn(move || loop {
		let msg: anyhow::Result<S2C> = read_frame(&mut read_stream);
		let Ok(msg) = msg else { break };
		let ev = match msg {
			S2C::AssignStart(a) => NetEvent::AssignStart(a),
			S2C::TickInputs(t) => NetEvent::TickInputs(t),
		};
		if tx_evt.send(ev).is_err() {
			break;
		}
	});

	// Writer
	thread::spawn(move || {
		while let Ok(cmd) = rx_cmd.recv() {
			match cmd {
				NetCmd::SendInput { tick, bits } => {
					let _ = write_frame(&mut write_stream, &C2S::Input(crate::protocol::InputMsg { tick, bits }));
				}
			}
		}
	});

	Ok((rx_evt, tx_cmd))
}


