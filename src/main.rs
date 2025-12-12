mod net;
mod protocol;
mod sim;

use std::{
	collections::VecDeque,
	sync::{
		Arc,
		atomic::{AtomicU32, Ordering},
	},
	time::{Duration, Instant},
};

use anyhow::Context;
use clap::{Parser, ValueEnum};
use macroquad::{miniquad::window::set_window_size, prelude::*};

use crate::{
	net::{NetCmd, NetEvent},
	sim::{InputBits, SimState, lerp},
};

// Server intentionally runs behind clock time by this many ticks
const LEAD_TICKS: u32 = 4;

// Server accepts client inputs for ticks in [server_tick, server_tick + D_MAX]
const D_MAX: u32 = 32;

// Rollback history size
const HISTORY: usize = 2048;

// Max ticks we simulate in one rendered frame
const CATCHUP_BUDGET_TICKS: u32 = 2000;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Runtime {
	Server,
	Client,
	Malicious,
}

#[derive(Debug, Parser)]
#[command(
	name = "repl-net-rs",
	about = "inputs-only deterministic networking demo"
)]
struct Args {
	#[arg(long, value_enum, default_value_t = Runtime::Client)]
	runtime: Runtime,

	#[arg(long, default_value = "127.0.0.1:4000")]
	addr: String,
}

fn draw_buffer_to_screen(buffer: &RenderTarget) {
	let sw = screen_width();
	let sh = screen_height();
	let scale = (sw / sim::BUFFER_W as f32).min(sh / sim::BUFFER_H as f32);
	let draw_w = sim::BUFFER_W as f32 * scale;
	let draw_h = sim::BUFFER_H as f32 * scale;
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
}

fn schedule_with_delay<T>(
	queue: &mut VecDeque<(Instant, T)>,
	last_scheduled_at: &mut Option<Instant>,
	msg: T,
	delay_ms: u32,
) {
	let now = Instant::now();
	let desired = now + Duration::from_millis(delay_ms as u64);
	let scheduled = match *last_scheduled_at {
		Some(prev) if prev > desired => prev,
		_ => desired,
	};
	*last_scheduled_at = Some(scheduled);
	queue.push_back((scheduled, msg));
}

#[macroquad::main("Demo")]
async fn main() -> anyhow::Result<()> {
	let args = Args::parse();

	let buffer = render_target(sim::BUFFER_W, sim::BUFFER_H);
	buffer.texture.set_filter(FilterMode::Nearest);
	set_window_size(sim::BUFFER_W * 3, sim::BUFFER_H * 3);

	match args.runtime {
		Runtime::Server => run_server(args.addr, buffer).await,
		Runtime::Client => run_client(args.addr, buffer, false).await,
		Runtime::Malicious => run_client(args.addr, buffer, true).await,
	}
}

async fn run_server(addr: String, buffer: RenderTarget) -> anyhow::Result<()> {
	let rx_render = net::spawn_server(addr, Duration::from_millis(800), LEAD_TICKS, D_MAX);
	let mut latest = net::ServerRender {
		tick: 0,
		state: SimState::new(),
	};

	loop {
		while let Ok(r) = rx_render.try_recv() {
			latest = r;
		}

		// Draw gameplay into the low-res buffer
		let mut cam = sim::camera_for_buffer();
		cam.render_target = Some(buffer.clone());
		set_camera(&cam);
		clear_background(BLACK);
		for (i, p) in latest.state.players.iter().enumerate() {
			let color = if i == 0 { BLUE } else { RED };
			draw_rectangle(p.x, p.y, sim::Player::W, sim::Player::H, color);
		}

		// Blit buffer to screen
		set_default_camera();
		clear_background(BLACK);
		draw_buffer_to_screen(&buffer);
		draw_text(
			&format!("server tick {}", latest.tick),
			10.0,
			24.0,
			16.0,
			WHITE,
		);

		next_frame().await;
	}
}

async fn run_client(addr: String, buffer: RenderTarget, malicious: bool) -> anyhow::Result<()> {
	let artificial_delay_ms = Arc::new(AtomicU32::new(0));
	let (rx_evt, tx_cmd) = net::spawn_client(addr).context("spawn_client")?;

	// Delay queues
	let mut in_q: VecDeque<(Instant, NetEvent)> = VecDeque::new();
	let mut out_q: VecDeque<(Instant, NetCmd)> = VecDeque::new();
	let mut in_last: Option<Instant> = None;
	let mut out_last: Option<Instant> = None;

	// Rolling history for rollback
	let mut auth_inputs: Vec<Option<(u32, [InputBits; sim::PLAYER_COUNT])>> = vec![None; HISTORY];
	let mut used_inputs: Vec<Option<(u32, [InputBits; sim::PLAYER_COUNT])>> = vec![None; HISTORY];
	let mut state_history: Vec<Option<(u32, SimState)>> = vec![None; HISTORY];

	let mut my_id: usize = 0;
	let mut sim_start_at: Option<Instant> = None;

	let mut state = SimState::new();
	let mut render_prev_state = state;
	let mut local_tick: u32 = 0;

	let mut last_remote: [InputBits; sim::PLAYER_COUNT] = [InputBits::empty(), InputBits::empty()];
	let mut latest_server_tick: u32 = 0;
	let mut pending_rollback: Option<u32> = None;

	let mut accumulator: f32 = 0.0;

	loop {
		if is_key_pressed(KeyCode::Left) {
			let cur = artificial_delay_ms.load(Ordering::Relaxed);
			artificial_delay_ms.store(cur.saturating_sub(10), Ordering::Relaxed);
		}

		if is_key_pressed(KeyCode::Right) {
			let cur = artificial_delay_ms.load(Ordering::Relaxed);
			artificial_delay_ms.store(cur.saturating_add(10), Ordering::Relaxed);
		}

		// Pull raw network events and schedule inbound delay
		while let Ok(ev) = rx_evt.try_recv() {
			match ev {
				NetEvent::AssignStart(_) => in_q.push_back((Instant::now(), ev)),
				NetEvent::TickInputs(_) => schedule_with_delay(
					&mut in_q,
					&mut in_last,
					ev,
					artificial_delay_ms.load(Ordering::Relaxed),
				),
			}
		}

		// Flush outbound delayed commands
		let now = Instant::now();
		while let Some((send_at, _)) = out_q.front() {
			if *send_at > now {
				break;
			}
			let (_, cmd) = out_q.pop_front().unwrap();
			let _ = tx_cmd.send(cmd);
		}

		// Deliver inbound events whose delay has elapsed
		let now = Instant::now();
		while let Some((deliver_at, _)) = in_q.front() {
			if *deliver_at > now {
				break;
			}
			let (_, ev) = in_q.pop_front().unwrap();
			match ev {
				NetEvent::AssignStart(a) => {
					my_id = a.player_id as usize;
					sim_start_at =
						Some(Instant::now() + Duration::from_millis(a.start_after_ms as u64));

					state = SimState::new();
					render_prev_state = state;
					local_tick = 0;
					latest_server_tick = 0;
					pending_rollback = None;
					accumulator = 0.0;
					in_q.clear();
					out_q.clear();
					in_last = None;
					out_last = None;
					last_remote = [InputBits::empty(), InputBits::empty()];
					for s in auth_inputs.iter_mut() {
						*s = None;
					}
					for s in used_inputs.iter_mut() {
						*s = None;
					}
					for s in state_history.iter_mut() {
						*s = None;
					}
				}
				NetEvent::TickInputs(m) => {
					latest_server_tick = latest_server_tick.max(m.tick);
					let idx = (m.tick as usize) % HISTORY;
					let inputs = [
						InputBits::from_u8(m.inputs[0]),
						InputBits::from_u8(m.inputs[1]),
					];
					auth_inputs[idx] = Some((m.tick, inputs));
					last_remote[0] = inputs[0];
					last_remote[1] = inputs[1];

					if let Some((t_used, used)) = used_inputs[idx] {
						if t_used == m.tick && used != inputs {
							pending_rollback = Some(match pending_rollback {
								Some(t0) => t0.min(m.tick),
								None => m.tick,
							});
						}
					}
				}
			}
		}

		// Wait for start
		let Some(start_at) = sim_start_at else {
			set_default_camera();
			clear_background(BLACK);
			draw_text("connecting...", 20.0, 30.0, 16.0, WHITE);
			next_frame().await;
			continue;
		};
		if Instant::now() < start_at {
			set_default_camera();
			clear_background(BLACK);
			draw_text("waiting for start...", 20.0, 30.0, 16.0, WHITE);
			next_frame().await;
			continue;
		}

		// If we detected an authoritative mismatch, rewind to that tick and replay
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

		// Determine where we should be by clock time
		let time_tick = (Instant::now()
			.saturating_duration_since(start_at)
			.as_secs_f32()
			* sim::TPS as f32)
			.floor() as u32;
		let target_tick = time_tick;

		// Estimated latency from the artificial delay
		let delay_ms = artificial_delay_ms.load(Ordering::Relaxed);
		let latency_ticks = ((delay_ms as f32 / 1000.0) * sim::TPS as f32).floor() as u32;

		// Estimated current server tick from shared time
		let server_tick_est = time_tick.saturating_sub(LEAD_TICKS);
		let max_stamp_tick = server_tick_est.saturating_add(D_MAX);

		// Time dilation to keep a healthy lead relative to the server timeline
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

		// Simulate forward (catch up if behind)
		let mut steps_this_frame: u32 = 0;
		while local_tick < target_tick
			&& steps_this_frame < CATCHUP_BUDGET_TICKS
			&& (accumulator >= sim::DT || local_tick + 1 < target_tick)
		{
			if accumulator < sim::DT {
				accumulator = sim::DT;
			}
			render_prev_state = state;

			let idx = (local_tick as usize) % HISTORY;
			state_history[idx] = Some((local_tick, state));

			let mut inputs = [InputBits::empty(), InputBits::empty()];
			let mut have_auth = false;
			if let Some((t, auth)) = auth_inputs[idx] {
				if t == local_tick {
					inputs = auth;
					have_auth = true;
				}
			}
			if !have_auth {
				for pid in 0..sim::PLAYER_COUNT {
					if pid == my_id {
						inputs[pid] = InputBits::from_keyboard();
					} else {
						inputs[pid] = last_remote[pid];
					}
				}
			}
			used_inputs[idx] = Some((local_tick, inputs));

			// Delay input submission by the same ms
			let stamped_tick = local_tick.saturating_add(latency_ticks).min(max_stamp_tick);
			schedule_with_delay(
				&mut out_q,
				&mut out_last,
				NetCmd::SendInput {
					tick: stamped_tick,
					bits: inputs[my_id].as_u8(),
				},
				delay_ms,
			);

			sim::step(&mut state, inputs);

			if malicious {
				state.players[my_id].y -= 20.0;
				state.players[my_id].vy = 0.0;
			}

			local_tick = local_tick.wrapping_add(1);
			accumulator -= sim::DT;
			steps_this_frame += 1;
		}

		// Render interpolation
		let alpha = (accumulator / sim::DT).clamp(0.0, 1.0);

		// Draw gameplay into low-res buffer
		let mut cam = sim::camera_for_buffer();
		cam.render_target = Some(buffer.clone());
		set_camera(&cam);
		clear_background(BLACK);
		for i in 0..sim::PLAYER_COUNT {
			let cur = state.players[i];
			let prev = render_prev_state.players[i];
			let x = lerp(prev.x, cur.x, alpha);
			let y = lerp(prev.y, cur.y, alpha);
			let color = if i == 0 { BLUE } else { RED };
			draw_rectangle(x, y, sim::Player::W, sim::Player::H, color);
		}

		// Blit buffer
		set_default_camera();
		clear_background(BLACK);
		draw_buffer_to_screen(&buffer);
		let title = if malicious { "malicious" } else { "client" };
		let delay = delay_ms;
		draw_text(
			&format!(
				"{title} id={my_id} tick={local_tick} srv={latest_server_tick} delay={delay}ms latency_ticks={latency_ticks}"
			),
			10.0,
			24.0,
			16.0,
			WHITE,
		);

		next_frame().await;
	}
}
