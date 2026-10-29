//! Tower-defense rules layered onto the original Survivors run: routed enemy waves, a defended
//! objective, and player-placed sentries. The world, unit attachment, spell combat, classes and
//! map selection remain the original game's systems.

use bevy::prelude::*;
use benilla_world::world_point::WorldPoint;

use super::abilities::Hit;
use super::data;
use super::enemies::{self, Enemy};
use super::hero::{self, AimPoint};
use super::units::{self, UnitSpec};
use super::{Phase, Rng, Run, Selection};
use crate::creature_anim::{move_flags, MovementState};

#[derive(Component, Clone)]
pub(super) struct RouteFollower {
    pub next: usize,
}

#[derive(Resource)]
pub(super) struct Objective {
    pub hp: f32,
    pub max_hp: f32,
    pub position: Vec3,
}
impl Default for Objective {
    fn default() -> Self { Self { hp: 1000.0, max_hp: 1000.0, position: Vec3::ZERO } }
}

#[derive(Resource)]
pub(super) struct Economy { pub gold: u32 }
impl Default for Economy { fn default() -> Self { Self { gold: 200 } } }

#[derive(Component)]
pub(super) struct RouteMarker;

#[derive(Resource, Default)]
pub(super) struct RouteVisualAssets {
    pub map_index: Option<usize>,
    path_mesh: Option<Handle<Mesh>>,
    path_material: Option<Handle<StandardMaterial>>,
    core_mesh: Option<Handle<Mesh>>,
    core_material: Option<Handle<StandardMaterial>>,
}

#[derive(Component)]
struct GoldAwarded;

#[derive(Component)]
pub(super) struct Tower { pub cooldown: f32, pub range: f32, pub damage: f32 }

#[derive(Resource, Default)]
pub(super) struct WaveSpawner { pub timer: f32, pub wave: u32 }

#[derive(Resource, Default)]
pub(super) struct RouteCache { pub map_index: usize, pub points: Vec<Vec3> }

const TOWER_COST: u32 = 50;
const TOWER_RANGE: f32 = 17.0;
const TOWER_DAMAGE: f32 = 28.0;

/// A readable, map-specific serpentine route placed relative to the selected map's real start
/// coordinates. Every battleground gets a different orientation and handedness.
pub(super) fn route_points(map_index: usize, map: &data::MapDef, world: &WorldPoint) -> Vec<Vec3> {
    let center = hero::arena(map);
    let scale = (map.leash / 80.0).clamp(0.55, 1.15);
    let mirror = if map_index % 2 == 0 { 1.0 } else { -1.0 };
    let bend = (map_index % 5) as f32 - 2.0;
    let offset = (map_index % 3) as f32 - 1.0;
    let local = [
        Vec2::new(offset * 3.0, 42.0),
        Vec2::new((17.0 + bend * 1.4) * mirror, 30.0),
        Vec2::new((-17.0 + offset * 2.0) * mirror, 17.0 + bend),
        Vec2::new((17.0 - bend * 1.2) * mirror, 4.0 - offset * 2.0),
        Vec2::new((-15.0 - bend) * mirror, -9.0 + offset * 2.0),
        Vec2::new(offset * 2.5, -24.0),
    ];
    local.iter().map(|p| {
        let p = *p * scale;
        let rotated = match map_index % 4 {
            1 => Vec2::new(p.y, -p.x),
            2 => Vec2::new(-p.x, -p.y),
            3 => Vec2::new(-p.y, p.x),
            _ => p,
        };
        let mut point = center + Vec3::new(rotated.x, 0.0, rotated.y);
        point.y = world.terrain_height_under(point).unwrap_or(center.y);
        point
    }).collect()
}

pub(super) fn refresh_route(
    mut commands: Commands,
    selection: Res<Selection>,
    world: WorldPoint,
    mut cache: ResMut<RouteCache>,
    mut objective: ResMut<Objective>,
    mut economy: ResMut<Economy>,
    mut waves: ResMut<WaveSpawner>,
    spawned: Query<Entity, Or<(With<Enemy>, With<Tower>, With<RouteMarker>)>>,
) {
    if cache.map_index == selection.stage && !cache.points.is_empty() { return; }
    for entity in &spawned { commands.entity(entity).despawn(); }
    cache.map_index = selection.stage;
    cache.points = route_points(selection.stage, selection.stage_def(), &world);
    objective.position = cache.points.last().copied().unwrap_or_else(|| hero::arena(selection.stage_def()));
    objective.hp = objective.max_hp;
    economy.gold = 200;
    *waves = WaveSpawner::default();
}


/// Build visible, terrain-following lane tiles and a stronghold marker with the same Bevy 3D
/// renderer used by the client world. The path is intentionally high contrast against the zone.
pub(super) fn refresh_route_visuals(
    mut commands: Commands,
    cache: Res<RouteCache>,
    world: WorldPoint,
    phase: Res<State<Phase>>,
    mut visuals: ResMut<RouteVisualAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if *phase.get() != Phase::Playing || visuals.map_index == Some(cache.map_index) || cache.points.len() < 2 { return; }
    let path_mesh = visuals.path_mesh.get_or_insert_with(|| meshes.add(Cuboid::new(1.55, 0.12, 1.55))).clone();
    let path_material = visuals.path_material.get_or_insert_with(|| materials.add(StandardMaterial {
        base_color: Color::srgb(0.12, 0.72, 0.34),
        unlit: true,
        ..default()
    })).clone();
    let core_mesh = visuals.core_mesh.get_or_insert_with(|| meshes.add(Cuboid::new(3.6, 4.2, 3.6))).clone();
    let core_material = visuals.core_material.get_or_insert_with(|| materials.add(StandardMaterial {
        base_color: Color::srgb(0.95, 0.62, 0.12),
        unlit: true,
        ..default()
    })).clone();

    for pair in cache.points.windows(2) {
        let start = pair[0];
        let end = pair[1];
        let distance = units::flat(end - start).length();
        let steps = (distance / 2.25).ceil().max(1.0) as usize;
        for i in 0..steps {
            let p = start.lerp(end, (i as f32 + 0.5) / steps as f32);
            let y = world.terrain_height_under(p).unwrap_or(p.y);
            commands.spawn((
                Mesh3d(path_mesh.clone()),
                MeshMaterial3d(path_material.clone()),
                Transform::from_translation(Vec3::new(p.x, y + 0.14, p.z)),
                RouteMarker,
            ));
        }
    }
    if let Some(goal) = cache.points.last().copied() {
        commands.spawn((
            Mesh3d(core_mesh),
            MeshMaterial3d(core_material),
            Transform::from_translation(goal + Vec3::Y * 2.1),
            RouteMarker,
        ));
    }
    visuals.map_index = Some(cache.map_index);
}

pub(super) fn toggle_route_visibility(
    phase: Res<State<Phase>>,
    mut markers: Query<&mut Visibility, With<RouteMarker>>,
) {
    let visibility = if *phase.get() == Phase::Playing { Visibility::Visible } else { Visibility::Hidden };
    for mut marker in &mut markers { *marker = visibility; }
}

/// Press T while aiming at open ground to place a sentry for 50 gold.
pub(super) fn place_tower(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    aim: Res<AimPoint>,
    selection: Res<Selection>,
    mut run: ResMut<Run>,
    mut economy: ResMut<Economy>,
    existing: Query<&Transform, With<Tower>>,
    cache: Res<RouteCache>,
    phase: Res<State<Phase>>,
) {
    if *phase.get() != Phase::Playing || !keys.just_pressed(KeyCode::KeyT) || economy.gold < TOWER_COST { return; }
    let Some(pos) = aim.0 else { return };
    let center = hero::arena(selection.stage_def());
    if units::flat(pos - center).length() > selection.stage_def().leash * 0.85
        || units::flat(pos - cache.points.last().copied().unwrap_or(center)).length() < 5.0
        || existing.iter().any(|tf| units::flat(tf.translation - pos).length() < 4.0)
        || cache.points.iter().any(|p| units::flat(*p - pos).length() < 4.0)
    { return; }
    // Use the selected class's real in-client display model so towers render through the same
    // WoW model attachment pipeline as the rest of the game rather than as placeholder geometry.
    let display = selection.class_def().map(|c| c.looks[0]).unwrap_or(0);
    let serial = run.guid();
    let e = units::spawn_unit(&mut commands, &UnitSpec {
        serial, display, scale: 0.8, level: 1, faction: units::FACTION_FRIENDLY,
        bytes0: 0, weapons: &[], sheath: 0,
    }, pos, 0.0);
    commands.entity(e).insert(Tower { cooldown: 0.1, range: TOWER_RANGE, damage: TOWER_DAMAGE });
    economy.gold -= TOWER_COST;
    run.banner = Some((format!("Sentry placed (-{} gold). Press T to place another.", TOWER_COST), 2.0));
}

pub(super) fn spawn_wave(
    mut commands: Commands,
    time: Res<Time>,
    mut run: ResMut<Run>,
    mut rng: ResMut<Rng>,
    mut waves: ResMut<WaveSpawner>,
    selection: Res<Selection>,
    cache: Res<RouteCache>,
    enemies_query: Query<&Enemy>,
) {
    if run.dead_t.is_some() || cache.points.len() < 2 { return; }
    waves.timer -= time.delta_secs();
    if waves.timer > 0.0 { return; }
    let alive = enemies_query.iter().filter(|e| !e.dead).count();
    if alive > 20 + waves.wave as usize * 2 {
        waves.timer = 0.5;
        return;
    }
    waves.wave += 1;
    waves.timer = (5.0 - waves.wave as f32 * 0.08).clamp(1.0, 5.0);
    let roster = data::roster(selection.stage);
    if roster.enemies.is_empty() { return; }
    let pool = data::wave_pool(run.elapsed, roster.enemies.len());
    let count = (5 + waves.wave as usize * 2).min(24);
    for _ in 0..count {
        let def = &roster.enemies[pool[rng.index(pool.len())]];
        let start = cache.points[0];
        let e = enemies::spawn_enemy(&mut commands, &mut run, def, start, cache.points[1], false);
        commands.entity(e).insert(RouteFollower { next: 1 });
    }
    if waves.wave % 5 == 0 && !roster.bosses.is_empty() {
        let def = &roster.bosses[((waves.wave / 5 - 1) as usize) % roster.bosses.len()];
        let e = enemies::spawn_enemy(&mut commands, &mut run, def, cache.points[0], cache.points[1], true);
        commands.entity(e).insert(RouteFollower { next: 1 });
        run.banner = Some((format!("{} is attacking the stronghold!", def.name), 4.0));
    } else {
        run.banner = Some((format!("Wave {} incoming!", waves.wave), 2.5));
    }
}

/// Follow waypoint segments rather than chasing the player. Existing creature movement flags
/// are retained so the same animation controller drives the models.
pub(super) fn move_creeps(
    time: Res<Time>,
    cache: Res<RouteCache>,
    mut creeps: Query<(&mut Transform, &mut Enemy, &mut RouteFollower, &mut MovementState)>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 || cache.points.len() < 2 { return; }
    for (mut tf, mut enemy, mut follower, mut movement) in &mut creeps {
        if enemy.dead { movement.speed = 0.0; movement.flags = 0; continue; }
        enemy.slow_t = (enemy.slow_t - dt).max(0.0);
        enemy.root_t = (enemy.root_t - dt).max(0.0);
        enemy.stun_t = (enemy.stun_t - dt).max(0.0);
        enemy.fear_t = (enemy.fear_t - dt).max(0.0);
        enemy.weaken_t = (enemy.weaken_t - dt).max(0.0);
        if enemy.slow_t <= 0.0 { enemy.slow = 1.0; }
        if enemy.weaken_t <= 0.0 { enemy.weaken = 1.0; }
        if follower.next >= cache.points.len() {
            movement.speed = 0.0;
            movement.flags = 0;
            continue;
        }
        if enemy.root_t > 0.0 || enemy.stun_t > 0.0 {
            movement.speed = 0.0;
            movement.flags = 0;
            continue;
        }
        let target = cache.points[follower.next];
        let delta = units::flat(target - tf.translation);
        let distance = delta.length();
        if distance < 1.2 {
            follower.next += 1;
            continue;
        }
        let direction = delta / distance;
        tf.translation += direction * enemy.speed * enemy.slow * dt.min(0.1);
        tf.rotation = Quat::from_rotation_y(units::yaw_toward(direction));
        movement.speed = enemy.speed * enemy.slow;
        movement.flags = move_flags::FORWARD;
    }
}

/// Kills fund the defense; a boss is worth enough for a replacement sentry.
pub(super) fn award_gold(
    mut commands: Commands,
    mut economy: ResMut<Economy>,
    dead: Query<(Entity, &Enemy, Has<GoldAwarded>)>,
) {
    for (entity, enemy, awarded) in &dead {
        if !enemy.dead || awarded { continue; }
        economy.gold = economy.gold.saturating_add(if enemy.boss { 75 } else { 5 });
        commands.entity(entity).insert(GoldAwarded);
    }
}

pub(super) fn tower_fire(
    time: Res<Time>,
    mut towers: Query<(&Transform, &mut Tower)>,
    enemies: Query<(Entity, &Transform, &Enemy)>,
    mut hits: MessageWriter<Hit>,
) {
    for (tf, mut tower) in &mut towers {
        tower.cooldown -= time.delta_secs();
        if tower.cooldown > 0.0 { continue; }
        let target = enemies.iter()
            .filter(|(_, _, enemy)| !enemy.dead)
            .filter_map(|(entity, enemy_tf, _)| {
                let d = units::flat(enemy_tf.translation - tf.translation).length();
                (d <= tower.range).then_some((entity, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((entity, _)) = target {
            hits.write(Hit::direct(entity, tower.damage, false));
            tower.cooldown = 0.8;
        } else { tower.cooldown = 0.15; }
    }
}

pub(super) fn leak_damage(
    mut commands: Commands,
    mut objective: ResMut<Objective>,
    mut run: ResMut<Run>,
    mut enemies: Query<(Entity, &Transform, &Enemy, &RouteFollower)>,
    cache: Res<RouteCache>,
) {
    let Some(goal) = cache.points.last().copied() else { return };
    for (entity, tf, enemy, follower) in &mut enemies {
        if enemy.dead || follower.next < cache.points.len() { continue; }
        if units::flat(tf.translation - goal).length() <= 1.7 {
            objective.hp = (objective.hp - if enemy.boss { 100.0 } else { 20.0 }).max(0.0);
            commands.entity(entity).despawn();
            if objective.hp <= 0.0 { run.dead_t = Some(run.elapsed); }
        }
    }
}

pub(super) fn objective_failure(
    objective: Res<Objective>,
    mut next: ResMut<NextState<Phase>>,
    state: Res<State<Phase>>,
) {
    if objective.hp <= 0.0 && *state.get() == Phase::Playing { next.set(Phase::GameOver); }
}
