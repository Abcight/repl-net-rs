use std::collections::HashMap;

use macroquad::{miniquad::window::set_window_size, prelude::*};

pub struct Player {
	x: f32,
	y: f32,
	vx: f32,
	vy: f32,
	local: bool,
	unique_id: u32,
}

pub enum PlayerInput {
	Jump,
	MoveLeft,
	MoveRight,
}

impl Player {
	pub const WIDTH: f32 = 32.0;
	pub const HEIGHT: f32 = 32.0;

	pub fn new(id: u32) -> Self {
		Self {
			x: 0.0,
			y: 0.0,
			vx: 0.0,
			vy: 0.0,
			local: false,
			unique_id: id,
		}
	}
}

struct GameRound {
	players: HashMap<u32, Player> // unique id to player
}

pub struct InputPackage {
	player_unique_id: u32,
	inputs: Vec<PlayerInput>,
}

#[macroquad::main("Demo")]
async fn main() {
	const WIDTH: u32 = 240;
	const HEIGHT: u32 = 140;

	let buffer = render_target(WIDTH, HEIGHT);
	buffer.texture.set_filter(FilterMode::Nearest);

	set_window_size(WIDTH * 3, HEIGHT * 3);

	let mut accumulator: f32 = 0.0;

	let mut game_state = GameRound {
		players: Default::default(),
	}; // net.join_round();

	let local_player_id = 0; // net.request_local_player_id();
	let local_player = Player {
		local: true,
		..Player::new(local_player_id)
	};

	let local_player_uid = local_player.unique_id;

	game_state.players.insert(local_player_id, local_player);

	loop {
		const TPS: u32 = 60;
		const SPT: f32 = 1.0 / TPS as f32;

		const GRAVITY: f32 = 600.0; // px/s^2

		const MOVE_SPEED: f32 = 90.0; // px/s
		const JUMP_SPEED: f32 = 220.0; // px/s (upwards)

		accumulator += get_frame_time();

		while accumulator > SPT {
			// process next tick
			let mut input_packages = { Vec::new() };// net.pop_queued_tick();

			// gather local inputs for this tick
			let mut inputs = Vec::new();

			if is_key_down(KeyCode::A) {
				inputs.push(PlayerInput::MoveLeft);
			}

			if is_key_down(KeyCode::D) {
				inputs.push(PlayerInput::MoveRight);
			}

			if is_key_down(KeyCode::Space) {
				inputs.push(PlayerInput::Jump);
			}

			let package = InputPackage {
				player_unique_id: local_player_uid,
				inputs
			};

			input_packages.push(package);

			// simulate inputs for players
			for input_package in input_packages {
				// received input for non-existent player, spawn them in
				let player_id = input_package.player_unique_id;
				let player = game_state
					.players
					.entry(player_id)
					.or_insert(Player::new(player_id));

				let mut dx: i32 = 0;

				// simulate all inputs
				for input in input_package.inputs {
					match input {
						PlayerInput::Jump => {
							let on_ground = player.y + Player::HEIGHT >= HEIGHT as f32;
							if on_ground {
								player.vy = -JUMP_SPEED;
							}
						},
						PlayerInput::MoveLeft => { dx -= 1; },
						PlayerInput::MoveRight => { dx += 1; },
					}
				}
				player.vx = (dx as f32) * MOVE_SPEED;
			}

			// simulate all players (perform physics etc.)
			for (_player_id, player) in &mut game_state.players {
				// gravity
				player.vy += GRAVITY * SPT;

				// integrate
				player.x += player.vx * SPT;
				player.y += player.vy * SPT;

				if player.x < 0.0 {
					player.x = 0.0;
				}
				let max_x = WIDTH as f32 - Player::WIDTH;
				if player.x > max_x {
					player.x = max_x;
				}

				if player.y < 0.0 {
					player.y = 0.0;
					if player.vy < 0.0 {
						player.vy = 0.0;
					}
				}

				let max_y = HEIGHT as f32 - Player::HEIGHT;
				if player.y > max_y {
					player.y = max_y;
					if player.vy > 0.0 {
						player.vy = 0.0;
					}
				}
			}

			accumulator -= SPT;
		}

		// draw the game state

		set_camera(&Camera2D {
			render_target: Some(buffer.clone()),
			zoom: vec2(2.0 / WIDTH as f32, 2.0 / HEIGHT as f32),
			target: vec2(WIDTH as f32 / 2.0, HEIGHT as f32 / 2.0),
			..Default::default()
		});
		
		clear_background(BLACK);

		for (_player_id, player) in &game_state.players {
			let color = match player.local {
				true => BLUE,
				false => RED
			};
			
			draw_rectangle(
				player.x,
				player.y,
				Player::WIDTH,
				Player::HEIGHT,
				color
			);
		}

		set_default_camera();

		clear_background(BLACK);

		let sw = screen_width();
		let sh = screen_height();

		let scale_x = sw / WIDTH as f32;
		let scale_y = sh / HEIGHT as f32;
		let scale = scale_x.min(scale_y);

		let draw_w = WIDTH as f32 * scale;
		let draw_h = HEIGHT as f32 * scale;

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

		next_frame().await
	}
}