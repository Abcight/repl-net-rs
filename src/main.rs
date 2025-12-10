use std::collections::HashMap;

use macroquad::prelude::*;

pub struct Player {
	x: f32,
	y: f32,
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
	loop {
		clear_background(BLACK);

		const TPS: u32 = 20;
		const SPT: f32 = 1.0 / TPS as f32;

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

				// simulate all inputs
				for input in input_package.inputs {
					// simulate with appropriate physics etc.
					match input {
						PlayerInput::Jump => {},
						PlayerInput::MoveLeft => {},
						PlayerInput::MoveRight => {},
					}
				}
			}

			accumulator -= SPT;
		}

		next_frame().await
	}
}