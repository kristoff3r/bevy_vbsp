use std::fmt;

use bevy::camera::Exposure;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::view::Hdr;
use bevy_bsp::{BspLoaderPlugin, MapAssets, spawn_map_entities};
use bevy_vpk::vpk::{LoadVPKDone, LoadVpks, VpkPlugin};

use clap::{Parser, ValueEnum};

#[derive(ValueEnum, Clone, Copy, Debug, Default)]
enum GamePreset {
    #[value(alias("tf2"))]
    TeamFortress2,
    #[value(alias("revolution"))]
    PortalRevolution,
    #[default]
    #[value(alias("css"))]
    CounterStrikeSource,
}

impl From<GamePreset> for Game {
    fn from(value: GamePreset) -> Self {
        match value {
            GamePreset::TeamFortress2 => Game::TF2,
            GamePreset::PortalRevolution => Game::PORTAL_REVOLUTION,
            GamePreset::CounterStrikeSource => Game::CSS,
        }
    }
}

impl fmt::Display for GamePreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GamePreset::TeamFortress2 => write!(f, "tf2"),
            GamePreset::PortalRevolution => write!(f, "revolution"),
            GamePreset::CounterStrikeSource => write!(f, "css"),
        }
    }
}

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value_t)]
    game: GamePreset,
    #[arg(default_value = "test")]
    map: String,
}

fn main() {
    let args = Args::parse();

    let game_preset = args.game;
    let game: Game = args.game.into();
    let map = args.map;
    let map_assets_path = format!("maps/{game_preset}/{map}.bsp");

    let load_vpks = move |mut commands: Commands| {
        commands.trigger(LoadVpks {
            paths: game.vpk_paths().into_iter().map(Into::into).collect(),
        });
    };

    let load_map =
        move |_event: On<LoadVPKDone>, mut commands: Commands, asset_server: Res<AssetServer>| {
            commands.insert_resource(MapAssets {
                bsp: asset_server.load(&map_assets_path),
            });
        };

    let mut app = App::new();
    // NOTE: VpkPlugin must come before DefaultPlugins due to registering an AssetSource
    app.add_plugins((VpkPlugin, DefaultPlugins, BspLoaderPlugin, PlayerPlugin));

    app.init_state::<MapState>();

    // app.add_systems(Startup, (load_vpks, spawn_light));
    app.add_systems(Startup, load_vpks);

    app.add_observer(load_map);

    app.add_systems(Update, check_map_loaded.run_if(in_state(MapState::Loading)));

    app.run();
}

#[cfg(windows)]
const PREFIX: &str = "C:/Program Files (x86)/Steam/steamapps/common";
#[cfg(target_os = "linux")]
const PREFIX: &str = concat!(env!("HOME"), "/.steam/steam/steamapps/common");
#[cfg(target_os = "macos")]
const PREFIX: &str = concat!(
    env!("HOME"),
    "/Library/Application Support/Steam/steamapps/common"
);

#[derive(Default, Copy, Clone)]
struct Game {
    name: &'static str,
    asset_dir: &'static str,
    vpk_prefix: Option<&'static str>,
    vpks: &'static [&'static str],
    extension: Option<&'static Game>,
}

const STANDARD_VPKS: [&str; 2] = ["textures", "misc"];

impl Game {
    const fn hl2(name: &'static str) -> Self {
        Game {
            name,
            asset_dir: "hl2",
            vpk_prefix: Some("hl2"),
            vpks: &STANDARD_VPKS,
            extension: None,
        }
    }

    const TF2: Game = Game {
        name: "Team Fortress 2",
        asset_dir: "tf",
        vpk_prefix: Some("tf2"),
        vpks: &STANDARD_VPKS,
        extension: Some(&Self::hl2("Team Fortress 2")),
    };

    const CSS: Game = Game {
        name: "Counter-Strike Source",
        asset_dir: "cstrike",
        vpk_prefix: Some("cstrike"),
        vpks: &["pak"],
        extension: Some(&Self::hl2("Counter-Strike Source")),
    };

    const PORTAL_REVOLUTION: Game = Game {
        name: "Portal Revolution",
        asset_dir: "revolution",
        vpk_prefix: None,
        vpks: &["pak01"],
        extension: None,
    };

    fn resolve(&self, vpk: &str) -> String {
        let Self {
            name,
            asset_dir,
            vpk_prefix,
            ..
        } = self;

        let vpk_name = match vpk_prefix {
            Some(prefix) => format_args!("{}_{vpk}", *prefix),
            None => format_args!("{vpk}"),
        };

        format!("{PREFIX}/{name}/{asset_dir}/{vpk_name}_dir.vpk")
    }

    fn vpk_paths(&self) -> Vec<String> {
        let mut paths = self
            .extension
            .map(|ext| ext.vpk_paths())
            .unwrap_or_default();

        paths.extend(self.vpks.iter().map(|vpk| self.resolve(vpk)));

        paths
    }
}

#[derive(States, Default, PartialEq, PartialOrd, Eq, Ord, Hash, Clone, Copy, Debug)]
pub enum MapState {
    #[default]
    Loading,
    Done,
}

fn spawn_light(mut commands: Commands) {
    commands.spawn((
        DirectionalLight {
            illuminance: light_consts::lux::AMBIENT_DAYLIGHT,
            shadows_enabled: true,
            affects_lightmapped_mesh_diffuse: false,
            ..default()
        },
        Transform::from_xyz(4.0, 7.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

fn check_map_loaded(
    mut commands: Commands,
    map_assets: Res<MapAssets>,
    asset_server: Res<AssetServer>,
    mut next_state: ResMut<NextState<MapState>>,
) {
    if asset_server.is_loaded(&map_assets.bsp) {
        commands.run_system_cached(spawn_map_entities);
        next_state.set(MapState::Done);
    }
}

// Inlined version of bevy_flycam
use bevy::ecs::message::MessageCursor;
use bevy::input::mouse::MouseMotion;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

pub mod prelude {
    pub use crate::*;
}

/// Keeps track of mouse motion events, pitch, and yaw
#[derive(Resource, Default)]
struct InputState {
    reader_motion: MessageCursor<MouseMotion>,
}

/// Mouse sensitivity and movement speed
#[derive(Resource)]
pub struct MovementSettings {
    pub sensitivity: f32,
    pub speed: f32,
}

impl Default for MovementSettings {
    fn default() -> Self {
        Self {
            sensitivity: 0.00012,
            speed: 12.,
        }
    }
}

/// Key configuration
#[derive(Resource)]
pub struct KeyBindings {
    pub move_forward: KeyCode,
    pub move_backward: KeyCode,
    pub move_left: KeyCode,
    pub move_right: KeyCode,
    pub move_ascend: KeyCode,
    pub move_descend: KeyCode,
    pub toggle_grab_cursor: KeyCode,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            move_forward: KeyCode::KeyW,
            move_backward: KeyCode::KeyS,
            move_left: KeyCode::KeyA,
            move_right: KeyCode::KeyD,
            move_ascend: KeyCode::Space,
            move_descend: KeyCode::ControlLeft,
            toggle_grab_cursor: KeyCode::Escape,
        }
    }
}

/// Used in queries when you want flycams and not other cameras
/// A marker component used in queries when you want flycams and not other cameras
#[derive(Component)]
pub struct FlyCam;

/// Grabs/ungrabs mouse cursor
fn toggle_grab_cursor(cursor_options: &mut CursorOptions) {
    match cursor_options.grab_mode {
        CursorGrabMode::None => {
            cursor_options.grab_mode = CursorGrabMode::Confined;
            cursor_options.visible = false;
        }
        _ => {
            cursor_options.grab_mode = CursorGrabMode::None;
            cursor_options.visible = true;
        }
    }
}

/// Grabs the cursor when game first starts
fn initial_grab_cursor(mut cursor_options: Query<&mut CursorOptions, With<PrimaryWindow>>) {
    if let Ok(mut cursor_options) = cursor_options.single_mut() {
        toggle_grab_cursor(&mut cursor_options);
    } else {
        warn!("Primary window not found for `initial_grab_cursor`!");
    }
}

/// Spawns the `Camera3dBundle` to be controlled
fn setup_player(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        Hdr,
        Bloom::default(),
        Exposure::INDOOR,
        Transform::from_xyz(-2.0, 5.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
        FlyCam,
    ));
}

/// Handles keyboard input and movement
fn player_move(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    cursor_options: Query<&CursorOptions, With<PrimaryWindow>>,
    settings: Res<MovementSettings>,
    key_bindings: Res<KeyBindings>,
    mut query: Query<(&FlyCam, &mut Transform)>, //    mut query: Query<&mut Transform, With<FlyCam>>,
) {
    if let Ok(cursor_options) = cursor_options.single() {
        for (_camera, mut transform) in query.iter_mut() {
            let mut velocity = Vec3::ZERO;
            let local_z = transform.local_z();
            let forward = -Vec3::new(local_z.x, 0., local_z.z);
            let right = Vec3::new(local_z.z, 0., -local_z.x);

            for key in keys.get_pressed() {
                match cursor_options.grab_mode {
                    CursorGrabMode::None => (),
                    _ => {
                        let key = *key;
                        if key == key_bindings.move_forward {
                            velocity += forward;
                        } else if key == key_bindings.move_backward {
                            velocity -= forward;
                        } else if key == key_bindings.move_left {
                            velocity -= right;
                        } else if key == key_bindings.move_right {
                            velocity += right;
                        } else if key == key_bindings.move_ascend {
                            velocity += Vec3::Y;
                        } else if key == key_bindings.move_descend {
                            velocity -= Vec3::Y;
                        }
                    }
                }

                velocity = velocity.normalize_or_zero();

                transform.translation += velocity * time.delta_secs() * settings.speed
            }
        }
    } else {
        warn!("Primary window not found for `player_move`!");
    }
}

/// Handles looking around if cursor is locked
fn player_look(
    settings: Res<MovementSettings>,
    cursor_options: Query<(&Window, &CursorOptions), With<PrimaryWindow>>,
    mut state: ResMut<InputState>,
    motion: Res<Messages<MouseMotion>>,
    mut query: Query<&mut Transform, With<FlyCam>>,
) {
    if let Ok((window, cursor_options)) = cursor_options.single() {
        for mut transform in query.iter_mut() {
            for ev in state.reader_motion.read(&motion) {
                let (mut yaw, mut pitch, _) = transform.rotation.to_euler(EulerRot::YXZ);
                match cursor_options.grab_mode {
                    CursorGrabMode::None => (),
                    _ => {
                        // Using smallest of height or width ensures equal vertical and horizontal sensitivity
                        let window_scale = window.height().min(window.width());
                        pitch -= (settings.sensitivity * ev.delta.y * window_scale).to_radians();
                        yaw -= (settings.sensitivity * ev.delta.x * window_scale).to_radians();
                    }
                }

                pitch = pitch.clamp(-1.54, 1.54);

                // Order is important to prevent unintended roll
                transform.rotation =
                    Quat::from_axis_angle(Vec3::Y, yaw) * Quat::from_axis_angle(Vec3::X, pitch);
            }
        }
    } else {
        warn!("Primary window not found for `player_look`!");
    }
}

fn cursor_grab(
    keys: Res<ButtonInput<KeyCode>>,
    key_bindings: Res<KeyBindings>,
    mut cursor_options: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    if let Ok(mut cursor_options) = cursor_options.single_mut() {
        if keys.just_pressed(key_bindings.toggle_grab_cursor) {
            toggle_grab_cursor(&mut cursor_options);
        }
    } else {
        warn!("Primary window not found for `cursor_grab`!");
    }
}

// Grab cursor when an entity with FlyCam is added
fn initial_grab_on_flycam_spawn(
    mut cursor_options: Query<&mut CursorOptions, With<PrimaryWindow>>,
    query_added: Query<Entity, Added<FlyCam>>,
) {
    if query_added.is_empty() {
        return;
    }

    if let Ok(cursor_options) = &mut cursor_options.single_mut() {
        toggle_grab_cursor(cursor_options);
    } else {
        warn!("Primary window not found for `initial_grab_cursor`!");
    }
}

/// Contains everything needed to add first-person fly camera behavior to your game
pub struct PlayerPlugin;
impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputState>()
            .init_resource::<MovementSettings>()
            .init_resource::<KeyBindings>()
            .add_systems(Startup, setup_player)
            .add_systems(Startup, initial_grab_cursor)
            .add_systems(Update, player_move)
            .add_systems(Update, player_look)
            .add_systems(Update, cursor_grab);
    }
}

/// Same as [`PlayerPlugin`] but does not spawn a camera
pub struct NoCameraPlayerPlugin;
impl Plugin for NoCameraPlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputState>()
            .init_resource::<MovementSettings>()
            .init_resource::<KeyBindings>()
            .add_systems(Startup, initial_grab_cursor)
            .add_systems(Startup, initial_grab_on_flycam_spawn)
            .add_systems(Update, player_move)
            .add_systems(Update, player_look)
            .add_systems(Update, cursor_grab);
    }
}
