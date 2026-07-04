use crate::util::n2m::{Relationship, RelationshipTarget};
use arrayvec::ArrayVec;
use bevy::camera::visibility::Visibility;
use bevy::ecs::lifecycle::Insert;
use bevy::ecs::observer::On;
use bevy::ecs::system::ParallelCommands;
use bevy::math::bounding::{Aabb3d, IntersectsVolume};
use bevy::math::primitives::HalfSpace;
use bevy::math::{Mat4, Vec3, Vec3A, prelude::*};
use bevy::transform::TransformPoint;
use bevy::{
    app::{Last, Plugin, PostUpdate, Startup},
    camera::{
        Camera3d,
        primitives::Aabb,
        visibility::{Layer, RenderLayers},
    },
    ecs::{
        component::Component,
        entity::{Entity, EntityHashMap, EntityHashSet},
        hierarchy::Children,
        query::{Changed, Has, Or, QueryFilter, With, Without},
        schedule::IntoScheduleConfigs as _,
        system::{Commands, EntityCommand, Local, Query},
        world::EntityWorldMut,
    },
    transform::components::GlobalTransform,
    ui::{Node, px, widget::Text},
    utils::Parallel,
};
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
            (
                ensure_camera_has_render_mask,
                calculate_visible_set,
                // make_empty_render_layers_invisible,
            )
                .chain(),
        )
        .add_systems(
            PostUpdate,
            update_inverse_global_transform::<With<VisChildren>>,
        )
        .add_systems(Last, recalculate_visleaf)
        .add_observer(remove_camera_leaf_on_disable_visibility)
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

#[derive(Component)]
pub struct LockViscluster;

#[derive(Component)]
pub struct CalculateVisleaf;

#[derive(Component)]
struct VisleafCalculated;

#[cfg(false)]
mod plane_side {
    struct PlaneSide {
        front: bool,
        back: bool,
    }

    // This can be optimised to only use a single plane, but this implementation means less maths to
    // review.
    fn aabb_plane_side(
        half_space: &HalfSpace,
        aabb: &Aabb,
        world_from_local: &Affine3A,
    ) -> PlaneSide {
        let inverted_half_space = HalfSpace::new(-half_space.normal_d());

        PlaneSide {
            front: !aabb.is_in_half_space(&inverted_half_space, world_from_local),
            back: !aabb.is_in_half_space(&half_space, world_from_local),
        }
    }
}

fn recalculate_visleaf(
    commands: ParallelCommands,
    roots: Query<Entity, With<VisTreeElements>>,
    tree: Query<(&InverseGlobalTransform, &VisChildren)>,
    dynamic_entities: Query<
        (
            Entity,
            &Aabb,
            &GlobalTransform,
            Option<&RelationshipTarget<VisibleFrom>>,
        ),
        (
            With<CalculateVisleaf>,
            Or<(Without<VisleafCalculated>, Changed<GlobalTransform>)>,
        ),
    >,
    vis_clusters: Query<
        (&RelationshipTarget<VisibleFrom>, &Aabb, &GlobalTransform),
        Without<VisChildren>,
    >,
    node_stack: Local<Parallel<Vec<Entity>>>,
) {
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

    fn transform_aabb(aabb: &Aabb, transform: impl TransformPoint) -> Aabb3d {
        Aabb3d::from_point_cloud(
            Isometry3d::default(),
            corners(aabb).map(|v| transform.transform_point(v)),
        )
    }

    dynamic_entities.par_iter().for_each_init(
        || node_stack.borrow_local_mut(),
        |node_stack, (entity, aabb, transform, visible_from)| {
            // HACK: Why is this necessary?
            const AABB_SLOP: Vec3A = Vec3A::splat(0.1);

            let aabb = Aabb {
                center: aabb.center,
                half_extents: aabb.half_extents + AABB_SLOP,
            };
            let transformed_aabb = transform_aabb(&aabb, *transform);

            if let Some(visible_from) = visible_from {
                commands.command_scope(|mut commands| {
                    for visibility_relationship in visible_from.collection().values().flatten() {
                        commands.entity(*visibility_relationship).try_despawn();
                    }
                });
            }

            for root in roots {
                node_stack.clear();
                node_stack.push(root);

                while let Some(node) = node_stack.pop() {
                    // TODO: Just using the vistree to calculate the leaves doesn't work, here we're essentially
                    // looking at every leaf and calculating cluster membership from AABB overlap. That works but
                    // it's still unclear why we can't just use the tree.
                    if let Ok((visible_from, node_aabb, node_transform)) = vis_clusters.get(node) {
                        let transformed_node_aabb = transform_aabb(node_aabb, *node_transform);

                        if !transformed_aabb.intersects(&transformed_node_aabb) {
                            continue;
                        }

                        commands.command_scope(|mut commands| {
                            commands.spawn(Visible::new(node, entity));
                            for viewer in visible_from.collection().keys() {
                                commands.spawn(Visible::new(*viewer, entity));
                            }
                        });

                        continue;
                    }

                    let Ok((_inverse_transform, cur_node)) = tree.get(node) else {
                        continue;
                    };

                    // HACK: The node tree doesn't seem to correctly reflect the plane side, need to figure out why.
                    node_stack.extend([cur_node.front, cur_node.back]);

                    // let side = aabb_plane_side(
                    //     &cur_node.midpoint,
                    //     &aabb,
                    //     &(Affine3A::from_mat4(inverse_transform.0.as_dmat4().as_mat4())
                    //         * transform.affine()),
                    // );

                    // if side.front {
                    //     node_stack.push(cur_node.front);
                    // }

                    // if side.back {
                    //     node_stack.push(cur_node.back);
                    // }
                }
            }

            commands.command_scope(|mut commands| {
                commands.entity(entity).insert(VisleafCalculated);
            });
        },
    );
}

struct InsertIfNotEqual<C>(C);

impl<C: Component + PartialEq> EntityCommand for InsertIfNotEqual<C> {
    type Out = ();

    fn apply(self, mut entity: EntityWorldMut) {
        let existing = entity.get::<C>();

        if existing != Some(&self.0) {
            entity.insert(self.0);
        }
    }
}

#[derive(Component)]
pub struct DisableVisibility;

enum InlineEntityMap<T, const INLINE_COUNT: usize> {
    Inline(ArrayVec<(Entity, T), INLINE_COUNT>),
    Heap(EntityHashMap<T>),
}

impl<T, const INLINE_COUNT: usize> InlineEntityMap<T, INLINE_COUNT> {
    const fn new() -> Self {
        Self::Inline(ArrayVec::new_const())
    }

    fn get(&self, key: Entity) -> Option<&T> {
        match self {
            Self::Inline(array_vec) => array_vec.iter().find_map(|(k, v)| (*k == key).then_some(v)),
            Self::Heap(entity_hash_map) => entity_hash_map.get(&key),
        }
    }

    fn insert(&mut self, key: Entity, value: T) {
        match self {
            Self::Inline(array_vec) => {
                if let Err(capacity_error) = array_vec.try_push((key, value)) {
                    let (key, value) = capacity_error.element();
                    self.spill();
                    self.insert(key, value);
                }
            }
            Self::Heap(entity_hash_map) => {
                entity_hash_map.insert(key, value);
            }
        }
    }

    fn spill(&mut self) {
        if let Self::Inline(v) = self {
            let allocated = v.drain(..).collect();

            *self = Self::Heap(allocated);
        }
    }
}

#[derive(Component)]
struct CameraLeaf {
    root_to_leaf: InlineEntityMap<Entity, 4>,
}

fn remove_camera_leaf_on_disable_visibility(
    event: On<Insert, DisableVisibility>,
    mut commands: Commands,
) {
    commands.entity(event.entity).try_remove::<CameraLeaf>();
}

#[expect(clippy::too_many_arguments)]
fn calculate_visible_set(
    mut commands: Commands,
    cameras: Query<
        (
            Entity,
            &GlobalTransform,
            &CameraRenderMask,
            Option<&mut CameraLeaf>,
            Has<DisableVisibility>,
        ),
        (
            With<Camera3d>,
            Changed<GlobalTransform>,
            Without<LockViscluster>,
        ),
    >,
    mut elements: Query<
        (
            Entity,
            Option<&Children>,
            &mut RenderLayers,
            &mut Visibility,
        ),
        Or<(With<VisTreeElementOf>, With<VisleafCalculated>)>,
    >,
    mut visible_nodes: Local<EntityHashSet>,
    mut face_layers: Local<Parallel<Vec<(Entity, RenderLayers)>>>,
    roots: Query<Entity, With<VisTreeElements>>,
    tree: Query<(&InverseGlobalTransform, &VisChildren)>,
    vis_clusters: Query<&RelationshipTarget<VisibleEntities>>,
) {
    static EMPTY_PVS: EntityHashMap<EntityHashSet> = EntityHashMap::new();

    let all_camera_render_layers = cameras.iter().fold(RenderLayers::none(), |layers, camera| {
        layers.with(camera.2.0)
    });

    for (camera_entity, transform, camera_mask, mut previous_leaf, always_visible) in cameras {
        let camera_position = transform.transform_point(Vec3::ZERO).into();
        let camera_layer = RenderLayers::layer(camera_mask.0);

        let mut pending_previous_leaf = (previous_leaf.is_none() && !always_visible)
            .then_some(InlineEntityMap::<Entity, 4>::new());

        'calc_for_root: for root in roots {
            let cur_root_camera_leaf = previous_leaf
                .as_ref()
                .and_then(|leaf| leaf.root_to_leaf.get(root));
            visible_nodes.clear();

            let mut cur_ent = root;
            let pvs = loop {
                if Some(&cur_ent) == cur_root_camera_leaf {
                    continue 'calc_for_root;
                }

                if always_visible {
                    break &EMPTY_PVS;
                }

                let Ok((inverse_transform, cur_node)) = tree.get(cur_ent) else {
                    previous_leaf
                        .as_mut()
                        .map(|l| &mut l.root_to_leaf)
                        .or(pending_previous_leaf.as_mut())
                        .unwrap()
                        .insert(root, cur_ent);

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
                |face_layers, (entity, children, mut render_layers, mut visibility)| {
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

                    if *render_layers != new_layers {
                        *render_layers = new_layers.clone();
                    }

                    if !new_layers.intersects(&all_camera_render_layers) {
                        *visibility = Visibility::Hidden;
                    } else {
                        *visibility = Visibility::Inherited;

                        if let Some(children) = children {
                            face_layers.par_extend(
                                children
                                    .par_iter()
                                    .map(|entity| (*entity, new_layers.clone())),
                            );
                        }
                    }
                },
            );

            for (face_ent, layers) in face_layers.iter_mut().flat_map(|layers| layers.drain(..)) {
                commands.entity(face_ent).queue(InsertIfNotEqual(layers));
            }
        }

        if let Some(root_to_leaf) = pending_previous_leaf {
            commands
                .entity(camera_entity)
                .insert(CameraLeaf { root_to_leaf });
        }
    }
}
