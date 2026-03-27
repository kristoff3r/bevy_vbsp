use bevy::{
    app::{Plugin, PostUpdate, Startup},
    camera::{
        Camera3d,
        primitives::{Aabb, HalfSpace},
        visibility::{Layer, RenderLayers},
    },
    color::{Alpha, Hsva},
    ecs::{
        component::Component,
        entity::{Entity, EntityHashMap, EntityHashSet},
        hierarchy::Children,
        query::{Changed, With, Without},
        schedule::IntoScheduleConfigs as _,
        system::{Commands, In, Local, Query, Single},
    },
    gizmos::gizmos::Gizmos,
    math::Isometry3d,
    transform::components::GlobalTransform,
    ui::{Node, px, widget::Text},
};
use glam::{Quat, Vec2, Vec3, Vec3A, Vec3Swizzles as _};
use rand::{RngExt as _, SeedableRng, rngs::SmallRng};
use tracing::debug;

#[derive(Component)]
pub struct VisClusters {
    pub visibility_map: EntityHashMap<EntityHashSet>,
}

#[derive(Component)]
#[relationship(relationship_target = VisTreeElements)]
pub struct VisTreeElementOf {
    #[relationship]
    pub root: Entity,
}

#[derive(Component)]
pub struct VisChildren {
    pub front: Entity,
    pub back: Entity,
    pub midpoint: HalfSpace,
}

#[derive(Component, Debug)]
pub struct DebugViscluster(pub Option<u32>);

#[derive(Component)]
#[relationship_target(relationship = VisTreeElementOf, linked_spawn)]
pub struct VisTreeElements(EntityHashSet);

// TODO: Better name
#[derive(Debug, Clone, PartialEq)]
pub struct SetVisibleFromInput {
    pub position: Vec3A,
    pub layer: Layer,
}

#[derive(Component)]
pub struct CameraRenderMask(pub Layer);

pub struct VisdataPlugin;

impl Plugin for VisdataPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        app.add_systems(
            PostUpdate,
            (ensure_camera_has_render_mask, set_visible_from_cameras).chain(),
        )
        .add_systems(PostUpdate, debug_vis_planes)
        .add_systems(Startup, |mut commands: Commands| {
            commands.spawn((
                Node {
                    position_type: bevy::ui::PositionType::Absolute,
                    left: px(30),
                    top: px(30),
                    ..Default::default()
                },
                Text(Default::default()),
                DebugTextOverlay,
            ));
        });
    }
}

fn ensure_camera_has_render_mask(
    mut commands: Commands,
    with_layers: Query<(&CameraRenderMask, &mut RenderLayers), Changed<RenderLayers>>,
    without_layers: Query<(Entity, &CameraRenderMask), Without<RenderLayers>>,
) {
    for (mask, mut layers) in with_layers {
        if !layers.intersects(&RenderLayers::layer(mask.0)) {
            let new_layers = std::mem::take(&mut *layers).with(mask.0);
            *layers = new_layers;
        }
    }

    for (ent, mask) in without_layers {
        commands.entity(ent).insert(RenderLayers::layer(mask.0));
    }
}

fn in_front(halfspace: HalfSpace, vec: Vec3A) -> bool {
    Aabb {
        center: vec,
        half_extents: Vec3A::ZERO,
    }
    .is_in_half_space_identity(&halfspace)
}

#[derive(Component, Debug, Copy, Clone)]
struct DebugTextOverlay;

fn debug_vis_planes(
    camera: Option<Single<&GlobalTransform, (With<Camera3d>, Without<LockViscluster>)>>,
    roots: Query<Entity, With<VisTreeElements>>,
    tree: Query<(&GlobalTransform, &VisChildren)>,
    debug_visclusters: Query<&DebugViscluster>,
    mut debug_overlay: Single<&mut Text, With<DebugTextOverlay>>,
) {
    struct TreeEnt {
        entity: Entity,
        active: bool,
    }

    let Some(camera) = camera else {
        return;
    };

    // let mut rng = SmallRng::seed_from_u64(0);

    for root in roots {
        let mut nodes = vec![TreeEnt {
            entity: root,
            active: true,
        }];

        while let Some(cur_node) = nodes.pop() {
            let Ok((
                node_transform,
                VisChildren {
                    front,
                    back,
                    midpoint,
                },
            )) = tree.get(cur_node.entity)
            else {
                // Reached leaf.
                if cur_node.active {
                    let debug_viscluster = if let Ok(DebugViscluster(Some(cluster))) =
                        debug_visclusters.get(cur_node.entity)
                    {
                        format!("Viscluster: {cluster}")
                    } else {
                        format!("Viscluster: none")
                    };
                    debug_overlay.0 = debug_viscluster;
                }

                continue;
            };

            // let rotation =
            //     Quat::look_to_rh(midpoint.normal().into(), midpoint.normal().zxy().into());
            // let translation =
            //     node_transform.transform_point((midpoint.normal() * midpoint.d()).into());
            // let color = Hsva::hsv(rng.random_range(0f32..360f32), 0.8, 1.)
            //     .with_alpha(if cur_node.active { 0.4 } else { 0.0 });

            // gizmos.grid(
            //     Isometry3d {
            //         rotation,
            //         translation,
            //     },
            //     [20, 20].into(),
            //     Vec2::splat(1.),
            //     color,
            // );

            let inverse_transform = node_transform.to_matrix().inverse();

            let in_front = in_front(
                *midpoint,
                inverse_transform
                    .mul_mat4(&camera.to_matrix())
                    .transform_point3a(Vec3A::ZERO),
            );

            // let arrow_point = node_transform.transform_point(
            //     if in_front {
            //         midpoint.normal() * crate::SCALE.recip()
            //     } else {
            //         -midpoint.normal() * crate::SCALE.recip()
            //     }
            //     .into(),
            // );

            // gizmos.arrow(
            //     translation.into(),
            //     (translation + arrow_point).into(),
            //     color,
            // );

            let (active, inactive) = if in_front {
                (front, back)
            } else {
                (back, front)
            };

            nodes.extend([
                TreeEnt {
                    entity: *active,
                    active: true,
                },
                TreeEnt {
                    entity: *inactive,
                    active: false,
                },
            ])
        }
    }
}

#[derive(Component)]
pub struct LockViscluster;

fn set_visible_from_cameras(
    mut commands: Commands,
    cameras: Query<
        (&GlobalTransform, &CameraRenderMask),
        (
            With<Camera3d>,
            Changed<GlobalTransform>,
            Without<LockViscluster>,
        ),
    >,
) {
    for (transform, camera_mask) in cameras {
        commands.run_system_cached_with(
            set_visible_from,
            SetVisibleFromInput {
                position: transform.transform_point(Vec3::ZERO).into(),
                layer: camera_mask.0,
            },
        );
    }
}

fn set_visible_from(
    In(input): In<SetVisibleFromInput>,
    roots: Query<(Entity, &VisClusters), With<VisTreeElements>>,
    tree: Query<(&GlobalTransform, &VisChildren)>,
    elements: Query<(Entity, &Children, &mut RenderLayers), With<VisTreeElementOf>>,
    mut layers: Query<&mut RenderLayers, Without<VisTreeElementOf>>,
    mut visible_nodes: Local<EntityHashSet>,
) {
    visible_nodes.clear();

    for (root, clusters) in roots {
        let mut cur_ent = root;
        loop {
            let Ok((node_transform, cur_node)) = tree.get(cur_ent) else {
                if let Some(visible_entities) = clusters.visibility_map.get(&cur_ent) {
                    visible_nodes.extend(visible_entities.iter().copied());
                }
                break;
            };
            visible_nodes.insert(cur_ent);
            let inverse_transform = node_transform.to_matrix().inverse();

            let in_front = in_front(
                cur_node.midpoint,
                inverse_transform.transform_point3a(input.position),
            );

            cur_ent = if in_front {
                cur_node.front
            } else {
                cur_node.back
            };
        }
    }

    let camera_layer = RenderLayers::layer(input.layer);

    for (entity, children, mut render_layers) in elements {
        let visible = visible_nodes.contains(&entity);

        // Optimisation so we don't recalculate render layers unless necessary.
        if visible == render_layers.intersects(&camera_layer) {
            continue;
        }

        let new_layers = if visible {
            render_layers.clone().with(input.layer)
        } else {
            render_layers.clone().without(input.layer)
        };

        for entity in children {
            let Ok(mut child_layers) = layers.get_mut(*entity) else {
                debug!("Visdata child without elements: {entity}");
                continue;
            };

            // Optimisation so we don't recalculate render layers unless necessary.
            if *child_layers == new_layers {
                continue;
            }

            *child_layers = new_layers.clone();
        }

        *render_layers = new_layers;
    }
}
