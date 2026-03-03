pub mod formats;

use bevy::prelude::*;
use bevy_asset_loader::prelude::*;
use bevy_vpk::vpk::LoadVpks;

use crate::formats::bsp::{BspAsset, BspLoaderPlugin, spawn_map_entities};

pub use bevy_vpk;

pub struct MapPlugin;

const VPK_PATHS: &[&str] = &[
    "/home/kris/.steam/steam/steamapps/common/Counter-Strike Source/cstrike/cstrike_pak_dir.vpk",
    "/home/kris/.steam/steam/steamapps/common/Counter-Strike Source/hl2/hl2_textures_dir.vpk",
    "/home/kris/.steam/steam/steamapps/common/Counter-Strike Source/hl2/hl2_misc_dir.vpk",
];

#[derive(Event)]
pub struct LoadMap {
    pub path: String,
}

#[derive(PartialEq, PartialOrd, Debug, Hash, Eq, Clone, Copy, Default, States)]
pub enum MapLoadingState {
    #[default]
    None,
    Loading,
    Done,
}

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(BspLoaderPlugin);
        app.add_systems(Startup, load_vpks);

        // Loading
        app.init_state::<MapLoadingState>();
        app.add_loading_state(
            LoadingState::new(MapLoadingState::Loading)
                .continue_to_state(MapLoadingState::Done)
                .load_collection::<MapAssets>(),
        );
        app.add_systems(
            OnEnter(MapLoadingState::Done),
            (spawn_lighting, spawn_map_entities),
        );

        app.add_observer(
            |load: On<LoadMap>,
             mut dynamic_assets: ResMut<DynamicAssets>,
             mut state: ResMut<NextState<MapLoadingState>>| {
                info!("Loading map {}", load.path);
                dynamic_assets.register_asset(
                    "bsp",
                    Box::new(StandardDynamicAsset::File {
                        path: load.path.clone(),
                    }),
                );
                state.set(MapLoadingState::Loading);
            },
        );
    }
}

fn spawn_lighting(mut commands: Commands, ambient_light: Option<ResMut<GlobalAmbientLight>>) {
    commands.spawn((
        DirectionalLight {
            illuminance: light_consts::lux::AMBIENT_DAYLIGHT,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 7.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    if let Some(mut ambient_light) = ambient_light {
        ambient_light.brightness = 400.0;
    }
}

fn load_vpks(mut commands: Commands) {
    info!("Loading vpks");
    commands.trigger(LoadVpks {
        paths: VPK_PATHS.iter().map(|p| p.into()).collect(),
    });
}
