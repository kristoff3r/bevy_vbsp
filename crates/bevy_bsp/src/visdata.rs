use std::collections::HashSet;

use bevy::{
    app::{Plugin, PostUpdate},
    camera::{
        Camera3d,
        primitives::{Aabb, HalfSpace},
        visibility::{Layer, RenderLayers},
    },
    ecs::{
        component::Component,
        entity::{Entity, EntityHashMap, EntityHashSet},
        hierarchy::Children,
        query::{Changed, Has, With, Without},
        schedule::IntoScheduleConfigs as _,
        system::{Commands, In, Local, ParallelCommands, Query},
    },
    gizmos::gizmos::Gizmos,
    pbr::wireframe::Wireframe,
    transform::components::{GlobalTransform, Transform},
};
use glam::{Vec3, Vec3A};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use tracing::{debug, error, info, warn};

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
        );
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

fn set_visible_from_cameras(
    mut commands: Commands,
    // cameras: Query<
    //     (&GlobalTransform, &CameraRenderMask),
    //     (With<Camera3d>, Changed<GlobalTransform>),
    // >,
    cameras: Query<(&GlobalTransform, &CameraRenderMask), With<Camera3d>>,
) {
    for (transform, camera_mask) in cameras {
        commands.run_system_cached_with(
            set_visible_from,
            SetVisibleFromInput {
                position: transform.translation().into(),
                layer: camera_mask.0,
            },
        );
        commands.run_system_cached_with(
            set_debug_visible_from,
            SetVisibleFromInput {
                position: transform.transform_point(Vec3::ZERO).into(),
                layer: camera_mask.0,
            },
        );
    }
}

macro_rules! calc_visclusters {
    ($input:expr, $visible_nodes:expr, $roots:expr, $tree:expr) => {
        for (root, clusters) in $roots {
            let mut cur_ent = root;

            loop {
                let Ok(cur_node) = $tree.get(cur_ent) else {
                    if let Some(visible_entities) = clusters.visibility_map.get(&cur_ent) {
                        $visible_nodes.extend(visible_entities.iter().copied());
                    }

                    break;
                };

                $visible_nodes.insert(cur_ent);

                let p_normal = cur_node.midpoint.normal().as_dvec3();
                let signed_distance =
                    p_normal.dot($input.position.as_dvec3()) + cur_node.midpoint.d() as f64;
                cur_ent = if signed_distance <= 0. {
                    cur_node.front
                } else {
                    cur_node.back
                };
            }
        }
    };
}

fn set_debug_visible_from(
    In(input): In<SetVisibleFromInput>,
    mut commands: Commands,
    mut gizmos: Gizmos,
    roots: Query<(Entity, &VisClusters), With<VisTreeElements>>,
    tree: Query<&VisChildren>,
    mut elements: Query<
        (Entity, &Children, Has<Wireframe>),
        (With<VisTreeElementOf>, With<RenderLayers>),
    >,
    layers: Query<(&Aabb, Has<Wireframe>), (Without<VisTreeElementOf>, With<RenderLayers>)>,
    mut visible_nodes: Local<EntityHashSet>,
) {
    visible_nodes.clear();

    calc_visclusters!(input, visible_nodes, roots, tree);

    for (entity, children, has_wireframe) in elements {
        let visible = visible_nodes.contains(&entity);

        // Optimisation so we don't recalculate render layers unless necessary.
        // if visible == has_wireframe {
        //     return;
        // }

        let entity_render_layers = children
            .iter()
            .filter_map(|entity| {
                let Ok((transforms, child_layers)) = layers.get(*entity) else {
                    debug!("Visdata child without elements: {entity}");
                    return None;
                };

                // Optimisation so we don't recalculate render layers unless necessary.
                // if visible == child_layers {
                //     return Some(child_layers.clone());
                // }

                if visible {
                    gizmos.axes(Transform::from_translation(transforms.center.into()), 1.);
                    commands.entity(*entity).insert_if_new(Wireframe);
                } else {
                    commands.entity(*entity).try_remove::<Wireframe>();
                }

                Some(visible)
            })
            .reduce(|left, right| left || right)
            .unwrap_or_default();

        if entity_render_layers {
            commands.entity(entity).insert_if_new(Wireframe);
        } else {
            commands.entity(entity).try_remove::<Wireframe>();
        }
    }
}

fn set_visible_from(
    In(input): In<SetVisibleFromInput>,
    roots: Query<(Entity, &VisClusters), With<VisTreeElements>>,
    tree: Query<&VisChildren>,
    elements: Query<(Entity, &Children, &mut RenderLayers), With<VisTreeElementOf>>,
    mut layers: Query<&mut RenderLayers, Without<VisTreeElementOf>>,
    mut visible_nodes: Local<EntityHashSet>,
) {
    visible_nodes.clear();

    calc_visclusters!(input, visible_nodes, roots, tree);

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

    // elements
    //     .par_iter_mut()
    //     .for_each(|(entity, children, mut render_layers)| {
    //         let visible = visible_nodes.contains(&entity);

    //         // Optimisation so we don't recalculate render layers unless necessary.
    //         if visible == render_layers.intersects(&camera_layer) {
    //             return;
    //         }

    //         let new_layers = if visible {
    //             render_layers.clone().with(input.layer)
    //         } else {
    //             render_layers.clone().without(input.layer)
    //         };

    //         children.par_iter().for_each(|entity| {
    //             visited_s.send(*entity).unwrap();
    //             let Ok(child_layers) = layers.get(*entity) else {
    //                 debug!("Visdata child without elements: {entity}");
    //                 return;
    //             };

    //             // Optimisation so we don't recalculate render layers unless necessary.
    //             if *child_layers == new_layers {
    //                 return;
    //             }

    //             commands.command_scope(|mut commands| {
    //                 commands.entity(*entity).insert(new_layers.clone());
    //             });
    //         });

    //         *render_layers = new_layers;
    //     });
}
