use bevy::{
    app::{Last, Plugin, PostUpdate, Startup},
    camera::{
        Camera3d,
        primitives::{Aabb, HalfSpace},
        visibility::{Layer, RenderLayers},
    },
    ecs::{
        component::Component,
        entity::{Entity, EntityHashMap, EntityHashSet},
        hierarchy::Children,
        query::{Changed, Has, Or, QueryFilter, With, Without},
        schedule::IntoScheduleConfigs as _,
        system::{Commands, EntityCommand, Local, Query, Single},
        world::EntityWorldMut,
    },
    math::{
        Isometry3d,
        bounding::{Aabb3d, IntersectsVolume},
    },
    pbr::wireframe::Wireframe,
    transform::components::GlobalTransform,
    ui::{Node, px, widget::Text},
    utils::Parallel,
};
use bevy_n2m::{Relationship, RelationshipTarget};
use glam::{Affine3A, Mat4, Vec3, Vec3A};
use itertools::Itertools;
use rayon::iter::{IntoParallelRefIterator as _, ParallelExtend as _, ParallelIterator};

pub struct VisibleEntities;
pub struct VisibleFrom;

pub type Visible = Relationship<VisibleEntities, VisibleFrom>;

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
    pub always_visible: bool,
}

#[derive(Component)]
pub struct CameraRenderMask(pub Layer);

pub struct VisdataPlugin;

impl Plugin for VisdataPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        app.add_systems(
            PostUpdate,
            (ensure_camera_has_render_mask, calculate_visible_set).chain(),
        )
        .add_systems(
            PostUpdate,
            update_inverse_global_transform::<With<VisChildren>>,
        )
        .add_systems(Last, recalculate_visleaf)
        // .add_systems(PostUpdate, debug_vis_planes)
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

#[derive(Component)]
struct InverseGlobalTransform(Mat4);

fn update_inverse_global_transform<Filter: QueryFilter>(
    mut commands: Commands,
    transforms: Query<
        (
            Entity,
            &GlobalTransform,
            Option<&mut InverseGlobalTransform>,
        ),
        (
            Filter,
            Or<(Without<InverseGlobalTransform>, Changed<GlobalTransform>)>,
        ),
    >,
) {
    for (entity, transform, inverse) in transforms {
        let inverted_transform = transform.to_matrix().inverse();

        match inverse {
            Some(mut inverse) => inverse.0 = inverted_transform,
            None => {
                commands
                    .entity(entity)
                    .insert(InverseGlobalTransform(inverted_transform));
            }
        }
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

#[derive(Component)]
pub struct CalculateVisleaf;

#[derive(Component)]
struct VisleafCalculated;

struct PlaneSide {
    front: bool,
    back: bool,
}

// This can be optimised to only use a single plane, but this implementation means less maths to
// review.
fn aabb_plane_side(half_space: &HalfSpace, aabb: &Aabb, world_from_local: &Affine3A) -> PlaneSide {
    fn corners(aabb: &Aabb) -> impl Iterator<Item = Vec3> {
        let min_max = [aabb.min(), aabb.max()];
        let x = min_max.map(|v| v.x);
        let y = min_max.map(|v| v.y);
        let z = min_max.map(|v| v.z);

        x.into_iter()
            .cartesian_product(y)
            .cartesian_product(z)
            .map(|((x, y), z)| Vec3::new(x, y, z))
    }

    PlaneSide {
        front: corners(aabb)
            .map(|v| world_from_local.transform_point3(v))
            .any(|v| in_front(*half_space, v.into())),
        back: corners(aabb)
            .map(|v| world_from_local.transform_point3(v))
            .any(|v| !in_front(*half_space, v.into())),
    }
}

fn recalculate_visleaf(
    mut commands: Commands,
    roots: Query<Entity, With<VisTreeElements>>,
    tree: Query<(&InverseGlobalTransform, &VisChildren)>,
    dynamic_entities: Query<
        (
            Entity,
            &Aabb,
            &GlobalTransform,
            Option<&RelationshipTarget<VisibleFrom>>,
            Has<Wireframe>,
        ),
        (
            With<CalculateVisleaf>,
            Or<(Without<VisleafCalculated>, Changed<GlobalTransform>)>,
        ),
    >,
    leaves: Query<&RelationshipTarget<VisibleFrom>>,
    mut node_stack: Local<Vec<Entity>>,
) {
    for (entity, aabb, transform, visible_from, has_wireframe) in dynamic_entities {
        if let Some(visible_from) = visible_from {
            for visibility_relationship in visible_from.collection().values().flatten() {
                commands.entity(*visibility_relationship).try_despawn();
            }
        }

        for root in roots {
            node_stack.clear();
            node_stack.push(root);

            while let Some(node) = node_stack.pop() {
                let Ok((inverse_transform, cur_node)) = tree.get(node) else {
                    let Ok(visible_from) = leaves.get(node) else {
                        continue;
                    };

                    commands.spawn(Visible::new(node, entity));
                    for viewer in visible_from.collection().keys() {
                        if has_wireframe {
                            dbg!((entity, viewer));
                        }

                        commands.spawn(Visible::new(*viewer, entity));
                    }

                    continue;
                };

                fn corners(aabb: &Aabb) -> impl Iterator<Item = Vec3> {
                    let min_max = [aabb.min(), aabb.max()];
                    let x = min_max.map(|v| v.x);
                    let y = min_max.map(|v| v.y);
                    let z = min_max.map(|v| v.z);

                    x.into_iter()
                        .cartesian_product(y)
                        .cartesian_product(z)
                        .map(|((x, y), z)| Vec3::new(x, y, z))
                }

                let side = PlaneSide {
                    front: corners(aabb)
                        .map(|v| {
                            inverse_transform
                                .0
                                .transform_point3(transform.transform_point(v))
                        })
                        .any(|v| in_front(cur_node.midpoint, v.into())),
                    back: corners(aabb)
                        .map(|v| {
                            inverse_transform
                                .0
                                .transform_point3(transform.transform_point(v))
                        })
                        .any(|v| !in_front(cur_node.midpoint, v.into())),
                };

                if side.front {
                    node_stack.push(cur_node.front);
                }

                if side.back {
                    node_stack.push(cur_node.back);
                }
            }
        }

        commands.entity(entity).insert(VisleafCalculated);
    }
}

struct InsertIfNotEqual<C>(C);

impl<C: Component + PartialEq> EntityCommand for InsertIfNotEqual<C> {
    fn apply(self, mut entity: EntityWorldMut) -> () {
        let existing = entity.get::<C>();

        if existing != Some(&self.0) {
            entity.insert(self.0);
        }
    }
}

#[derive(Component)]
pub struct DisableVisibility;

fn calculate_visible_set(
    mut commands: Commands,
    cameras: Query<
        (&GlobalTransform, &CameraRenderMask, Has<DisableVisibility>),
        (
            With<Camera3d>,
            Changed<GlobalTransform>,
            Without<LockViscluster>,
        ),
    >,
    mut elements: Query<
        (Entity, Option<&Children>, &mut RenderLayers),
        Or<(With<VisTreeElementOf>, With<CalculateVisleaf>)>,
    >,
    mut visible_nodes: Local<EntityHashSet>,
    mut face_layers: Local<Parallel<Vec<(Entity, RenderLayers)>>>,
    roots: Query<Entity, With<VisTreeElements>>,
    tree: Query<(&InverseGlobalTransform, &VisChildren)>,
    vis_clusters: Query<&RelationshipTarget<VisibleEntities>>,
) {
    static EMPTY_PVS: EntityHashMap<EntityHashSet> = EntityHashMap::new();

    for (transform, camera_mask, always_visible) in cameras {
        let camera_position = transform.transform_point(Vec3::ZERO).into();
        let camera_layer = RenderLayers::layer(camera_mask.0);

        for root in roots {
            visible_nodes.clear();

            let mut cur_ent = root;
            let pvs = loop {
                if always_visible {
                    break &EMPTY_PVS;
                }

                let Ok((inverse_transform, cur_node)) = tree.get(cur_ent) else {
                    break vis_clusters
                        .get(cur_ent)
                        .map(|vis| vis.collection())
                        .unwrap_or(&EMPTY_PVS);
                };
                visible_nodes.insert(cur_ent);

                let in_front = in_front(
                    cur_node.midpoint,
                    inverse_transform.0.transform_point3a(camera_position),
                );

                cur_ent = if in_front {
                    cur_node.front
                } else {
                    cur_node.back
                };
            };

            elements.par_iter_mut().for_each_init(
                || face_layers.borrow_local_mut(),
                |face_layers, (entity, children, mut render_layers)| {
                    let visible = always_visible
                        || visible_nodes.contains(&entity)
                        || pvs.contains_key(&entity);

                    // Optimisation so we don't recalculate render layers unless necessary.
                    if visible == render_layers.intersects(&camera_layer) {
                        return;
                    }

                    let new_layers = if visible {
                        render_layers.clone().with(camera_mask.0)
                    } else {
                        render_layers.clone().without(camera_mask.0)
                    };

                    if let Some(children) = children {
                        face_layers.par_extend(
                            children
                                .par_iter()
                                .map(|entity| (*entity, new_layers.clone())),
                        );
                    }

                    *render_layers = new_layers;
                },
            );

            for (face_ent, layers) in face_layers.iter_mut().flat_map(|layers| layers.drain(..)) {
                commands.entity(face_ent).queue(InsertIfNotEqual(layers));
            }
        }
    }
}
