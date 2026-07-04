use core::marker::PhantomData;

use bevy::ecs::{
    entity::{EntityHashMap, EntityHashSet},
    lifecycle::HookContext,
    prelude::*,
    world::DeferredWorld,
};

/// The authoritative relationship record.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
#[component(immutable, on_insert = Self::on_insert, on_remove = Self::on_remove)]
pub struct Relationship<A, B, C = DefaultRelationshipCollection>
where
    A: Send + Sync + 'static,
    B: Send + Sync + 'static,
    C: RelationshipCollection,
{
    a: Entity,
    b: Entity,
    _marker: PhantomData<(A, B, C)>,
}

impl<A, B, C> Relationship<A, B, C>
where
    A: Send + Sync + 'static,
    B: Send + Sync + 'static,
    C: RelationshipCollection,
{
    pub fn new(a: Entity, b: Entity) -> Self {
        Self {
            a,
            b,
            _marker: PhantomData,
        }
    }

    fn on_insert(mut world: DeferredWorld<'_>, ctx: HookContext) {
        let &Self { a, b, _marker } = world
            .entity(ctx.entity)
            .get::<Self>()
            .expect("Relationship::on_insert hook called on entity that doesn't have Relationship");

        RelationshipTarget::<A, C>::increment_or(world.reborrow(), a, b, ctx.entity);
        RelationshipTarget::<B, C>::increment_or(world.reborrow(), b, a, ctx.entity);
    }

    fn on_remove(mut world: DeferredWorld<'_>, ctx: HookContext) {
        let &Self { a, b, _marker } = world
            .entity(ctx.entity)
            .get::<Self>()
            .expect("Relationship::on_remove hook called on entity that doesn't have Relationship");

        RelationshipTarget::<A, C>::decrement_or(world.reborrow(), a, b, ctx.entity);
        RelationshipTarget::<B, C>::decrement_or(world.reborrow(), b, a, ctx.entity);
    }
}

#[derive(Component)]
#[component(on_remove = Self::on_remove)]
pub struct RelationshipTarget<T, C = DefaultRelationshipCollection>
where
    T: Send + Sync + 'static,
    C: RelationshipCollection,
{
    collection: C,
    _phantom: PhantomData<T>,
}

impl<T, C> RelationshipTarget<T, C>
where
    T: Send + Sync + 'static,
    C: RelationshipCollection,
{
    fn new() -> Self {
        Self {
            collection: C::default(),
            _phantom: PhantomData,
        }
    }

    fn increment_or(
        mut world: DeferredWorld<'_>,
        entity: Entity,
        complementary_entity: Entity,
        relationship_entity: Entity,
    ) {
        if let Some(mut target) = world.entity_mut(entity).get_mut::<Self>() {
            target
                .collection
                .increment(complementary_entity, relationship_entity);
        } else {
            // The component isn't inserted yet, so we need to wait until we have exclusive access to the world.
            // If decremnt_or runs before the increment_or, it removes the component
            // (because it sees it empty) and then the increment_or will re-insert it.
            world.commands().queue(move |world: &mut World| {
                let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
                    return;
                };
                if let Some(mut target) = entity_mut.get_mut::<Self>() {
                    // The component was inserted in the meantime, just mutate it in place.
                    target
                        .collection
                        .increment(complementary_entity, relationship_entity);
                } else {
                    // The component still doesn't exist, so we add it.
                    let mut target = Self::new();
                    target
                        .collection
                        .increment(complementary_entity, relationship_entity);
                    entity_mut.insert(target);
                }
            });
        }
    }

    fn decrement_or(
        mut world: DeferredWorld<'_>,
        entity: Entity,
        complementary_entity: Entity,
        relationship_entity: Entity,
    ) {
        let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
            return;
        };
        let Some(mut target) = entity_mut.get_mut::<Self>() else {
            return;
        };

        target
            .collection
            .decrement(complementary_entity, relationship_entity);

        // Additionally, queue the component to be cleaned up if
        // it still exists when the command is run and is still empty.
        if target.collection.is_empty() {
            //  If increment_or runs first the remove will be a no-op.
            world.commands().queue(move |world: &mut World| {
                let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
                    return;
                };
                let Some(target) = entity_mut.get::<Self>() else {
                    return;
                };
                if target.collection.is_empty() {
                    entity_mut.remove::<Self>();
                }
            });
        }
    }

    fn on_remove(mut world: DeferredWorld<'_>, ctx: HookContext) {
        let (entities, mut commands) = world.entities_and_commands();

        let Ok(entity_ref) = entities.get(ctx.entity) else {
            return;
        };
        let Some(target) = entity_ref.get::<Self>() else {
            return;
        };

        for (_, relationship_entities) in target.collection().iter() {
            for relationship_entity in relationship_entities {
                // Queue the entity to be despawned. This has no invariants,
                // since the RelationshipTarget cannot suddenly be made "valid" again.
                // The relationship entity is guaranteed to be invalid.
                commands.entity(relationship_entity).despawn();
            }
        }
    }

    #[inline]
    pub fn collection(&self) -> &C {
        &self.collection
    }
}

/// A collection which stores the related complementary entities,
/// and the relationship entities establishing the relation.
pub trait RelationshipCollection: Send + Sync + Default + 'static {
    fn increment(&mut self, complementary_entity: Entity, relationship_entity: Entity);

    fn decrement(&mut self, complementary_entity: Entity, relationship_entity: Entity);

    fn iter(&self) -> impl Iterator<Item = (Entity, impl Iterator<Item = Entity>)>;

    fn len(&self) -> usize;

    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub type DefaultRelationshipCollection = EntityHashMap<EntityHashSet>;

impl RelationshipCollection for EntityHashMap<EntityHashSet> {
    fn increment(&mut self, complementary_entity: Entity, relationship_entity: Entity) {
        self.entry(complementary_entity)
            .or_default()
            .insert(relationship_entity);
    }

    fn decrement(&mut self, complementary_entity: Entity, relationship_entity: Entity) {
        let Some(set) = self.get_mut(&complementary_entity) else {
            return;
        };

        set.remove(&relationship_entity);

        if set.is_empty() {
            self.remove(&complementary_entity);
        }
    }

    fn iter(&self) -> impl Iterator<Item = (Entity, impl Iterator<Item = Entity>)> {
        (**self)
            .iter()
            .map(|(&entity, set)| (entity, set.iter().copied()))
    }

    #[inline]
    fn len(&self) -> usize {
        (**self).len()
    }
}
