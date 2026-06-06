use bevy::{
    color::palettes::tailwind::{PINK_100, RED_500},
    mesh::VertexAttributeValues,
    pbr::wireframe::{Wireframe, WireframeColor, WireframePlugin},
    picking::{
        Pickable,
        pointer::{PointerId, PointerInteraction},
    },
    prelude::*,
    window::{CursorGrabMode, CursorOptions, PrimaryWindow},
};

use crate::{
    BspAsset, MapAssets,
    entities::{BspBrushEntityMesh, BspEntityModelMesh, BspStaticPropMesh, BspWorldspawnMesh},
};

/// Plugin that adds BSP debug visualization.
/// Requires [`MeshPickingPlugin`] and a pointer that produces [`PointerInteraction`] hits.
pub struct BspDebugPlugin;

impl Plugin for BspDebugPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(WireframePlugin::default());
        app.init_resource::<HighlightedEntity>();
        app.add_systems(Startup, spawn_debug_panel);
        app.add_systems(Update, (draw_mesh_intersections, update_debug_panel));
    }
}

#[derive(Component)]
struct DebugPanel;

#[derive(Resource, Default)]
struct HighlightedEntity(Option<Entity>);

fn spawn_debug_panel(mut commands: Commands) {
    commands
        .spawn((
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(0.0),
                top: Val::Px(0.0),
                bottom: Val::Px(0.0),
                width: Val::Px(300.0),
                padding: UiRect::all(Val::Px(10.0)),
                overflow: Overflow::clip_y(),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)),
        ))
        .with_child((
            DebugPanel,
            Text::new("No mesh selected"),
            TextFont {
                font_size: 10.0,
                ..default()
            },
            TextColor(Color::WHITE),
        ));
}

fn draw_mesh_intersections(
    interactions: Query<(&PointerId, &PointerInteraction)>,
    cursor_options: Query<&CursorOptions, With<PrimaryWindow>>,
    mut gizmos: Gizmos,
) {
    let cursor_locked = cursor_options
        .single()
        .is_ok_and(|opts| opts.grab_mode != CursorGrabMode::None);

    for (id, interaction) in &interactions {
        if *id == PointerId::Mouse && cursor_locked {
            continue;
        }
        let Some((_entity, hit)) = interaction.get_nearest_hit() else {
            continue;
        };
        let (Some(point), Some(normal)) = (hit.position, hit.normal) else {
            continue;
        };
        gizmos.sphere(point, 0.05, RED_500);
        gizmos.arrow(point, point + normal.normalize() * 0.5, PINK_100);
    }
}

fn update_debug_panel(
    mut commands: Commands,
    interactions: Query<(&PointerId, &PointerInteraction)>,
    cursor_options: Query<&CursorOptions, With<PrimaryWindow>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    worldspawn: Query<&BspWorldspawnMesh>,
    brush_entity: Query<&BspBrushEntityMesh>,
    entity_model: Query<&BspEntityModelMesh>,
    static_prop: Query<&BspStaticPropMesh>,
    mesh_handles: Query<&Mesh3d>,
    meshes: Res<Assets<Mesh>>,
    map_assets: If<Res<MapAssets>>,
    bsp_assets: Res<Assets<BspAsset>>,
    mut panel: Query<&mut Text, With<DebugPanel>>,
    mut highlighted: ResMut<HighlightedEntity>,
) {
    let Ok(mut panel_text) = panel.single_mut() else {
        return;
    };

    let cursor_locked = cursor_options
        .single()
        .is_ok_and(|opts| opts.grab_mode != CursorGrabMode::None);

    let mut hit_entity = None;
    let mut texture_name = None;
    let mut info = String::new();

    for (id, interaction) in &interactions {
        if *id == PointerId::Mouse && cursor_locked {
            continue;
        }
        let Some((entity, hit)) = interaction.get_nearest_hit() else {
            continue;
        };
        if hit.position.is_none() || hit.normal.is_none() {
            continue;
        }

        hit_entity = Some(*entity);

        if let Ok(mesh) = worldspawn.get(*entity) {
            texture_name = Some(mesh.texture_name.clone());
            info = format!("Worldspawn\nTexture: {}", mesh.texture_name);
        } else if let Ok(mesh) = brush_entity.get(*entity) {
            texture_name = Some(mesh.texture_name.clone());
            info = format!(
                "Brush Entity\nClass: {}\nTexture: {}\nModel: *{}",
                mesh.classname, mesh.texture_name, mesh.model_index
            );
        } else if let Ok(mesh) = entity_model.get(*entity) {
            info = format!(
                "Entity Model\nClass: {}\nModel: {}",
                mesh.classname, mesh.model_path
            );
        } else if let Ok(mesh) = static_prop.get(*entity) {
            info = format!(
                "Static Prop\nModel: {}\nIndex: {}",
                mesh.model_path, mesh.prop_index
            );
        }

        // Show UV range of the mesh
        if let Ok(mesh_handle) = mesh_handles.get(*entity)
            && let Some(mesh) = meshes.get(&mesh_handle.0)
            && let Some(uvs) = mesh.attribute(Mesh::ATTRIBUTE_UV_0)
            && let VertexAttributeValues::Float32x2(uvs) = uvs
        {
            let (mut min_u, mut min_v) = (f32::MAX, f32::MAX);
            let (mut max_u, mut max_v) = (f32::MIN, f32::MIN);
            for [u, v] in uvs {
                min_u = min_u.min(*u);
                min_v = min_v.min(*v);
                max_u = max_u.max(*u);
                max_v = max_v.max(*v);
            }
            info.push_str(&format!(
                "\n\nUV range:\n  U: {min_u:.3}..{max_u:.3}\n  V: {min_v:.3}..{max_v:.3}"
            ));
        }

        break;
    }

    // Append VMT and VTF info if we have a texture name
    if let Some(ref tex_name) = texture_name
        && let Some(bsp_asset) = bsp_assets.get(&map_assets.bsp)
    {
        // Show BSP texture data dimensions (used for UV computation)
        if let Some(tex_data) = bsp_asset
            .bsp
            .textures()
            .find(|t| t.name().to_ascii_lowercase() == *tex_name)
        {
            let td = tex_data.texture_data();
            info.push_str(&format!(
                "\n\n--- BSP TexData ---\n{}x{} (view: {}x{})",
                td.width, td.height, td.view_width, td.view_height
            ));
        }
        if let Some(vtf) = bsp_asset.vtf_info.get(tex_name) {
            info.push_str(&format!(
                "\n\n--- VTF ---\nHeader: {}x{} Decoded: {}x{}\n{}\nFlags: {:?}",
                vtf.width, vtf.height, vtf.decoded_width, vtf.decoded_height, vtf.format, vtf.flags,
            ));
        }
        if let Some(vmt) = bsp_asset.vmt_materials.get(tex_name) {
            info.push_str(&format!("\n\n--- VMT ---\n{vmt:#?}"));

            if mouse_buttons.just_pressed(MouseButton::Left) {
                if let Some(vtf) = bsp_asset.vtf_info.get(tex_name) {
                    println!("=== {tex_name} ===\n{vtf:#?}\n{vmt:#?}\n");
                } else {
                    println!("=== {tex_name} ===\n{vmt:#?}\n");
                }
            }
        }
    }

    // Update wireframe highlight
    if highlighted.0 != hit_entity {
        if let Some(prev) = highlighted.0
            && let Ok(mut entity_commands) = commands.get_entity(prev)
        {
            entity_commands.remove::<(Wireframe, WireframeColor)>();
        }
        if let Some(new) = hit_entity {
            commands.entity(new).insert((
                Wireframe,
                WireframeColor {
                    color: Color::srgb(1.0, 0.5, 0.0),
                },
            ));
        }
        highlighted.0 = hit_entity;
    }

    if info.is_empty() {
        info = "No mesh selected".to_string();
    }
    **panel_text = info;
}
