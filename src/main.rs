mod input;
mod output;

use input::{DAY, WEEK, chunks, digits, fast_timestamp_f1, fields, lines, maps, timestamp};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

const BUCKETS: usize = 16;
const EVENT_BUCKETS: usize = 256;

#[inline]
fn shard(key: &[u8]) -> usize {
    let mut hasher = FxHasher::default();
    key.hash(&mut hasher);
    let hash = hasher.finish();
    ((hash ^ (hash >> 29)) as usize) & (BUCKETS - 1)
}

type Name<'a> = &'a [u8];

struct People<'a> {
    names: FxHashMap<Name<'a>, Name<'a>>,
}

impl<'a> People<'a> {
    #[inline]
    fn name(&self, name: Name<'a>) -> Name<'a> {
        if self.names.is_empty() {
            name
        } else {
            self.names.get(name).copied().unwrap_or(name)
        }
    }

    fn from_links(files: &'a [memmap2::Mmap]) -> Self {
        fn root(parent: &mut [u32], mut id: u32) -> u32 {
            while parent[id as usize] != id {
                parent[id as usize] = parent[parent[id as usize] as usize];
                id = parent[id as usize];
            }
            id
        }

        let mut ids = FxHashMap::with_capacity_and_hasher(1_000_000, Default::default());
        let mut labels = Vec::with_capacity(1_000_000);
        let mut parent = Vec::with_capacity(1_000_000);
        let mut sizes = Vec::with_capacity(1_000_000);
        let raw_pairs: Vec<[Name<'a>; 2]> = chunks(files)
            .into_par_iter()
            .flat_map_iter(|chunk| {
                lines(chunk).filter_map(input::link_fields)
            })
            .collect();

        for pair in raw_pairs {
            let [a, b] = pair.map(|name| {
                *ids.entry(name).or_insert_with(|| {
                    let id = labels.len() as u32;
                    labels.push(name);
                    parent.push(id);
                    sizes.push(1_u32);
                    id
                })
            });
            let mut a = root(&mut parent, a) as usize;
            let mut b = root(&mut parent, b) as usize;
            if a == b {
                continue;
            }
            if sizes[a] < sizes[b] {
                std::mem::swap(&mut a, &mut b);
            }
            parent[b] = a as u32;
            sizes[a] += sizes[b];
            labels[a] = labels[a].min(labels[b]);
        }
        let names = ids
            .into_iter()
            .filter_map(|(id, node)| {
                let name = labels[root(&mut parent, node) as usize];
                (id != name).then_some((id, name))
            })
            .collect();
        Self { names }
    }
}

struct Catalog<'a> {
    category: FxHashMap<Name<'a>, u32>,
    categories: Vec<Name<'a>>,
    campaign: FxHashMap<Name<'a>, u32>,
    campaigns: Vec<(Name<'a>, Name<'a>)>,
}

impl<'a> Catalog<'a> {
    fn new(products: &'a [u8], campaigns: &'a [u8]) -> Self {
        let mut category = FxHashMap::default();
        let mut labels = FxHashMap::default();
        let mut categories = vec![b"unknown".as_slice()];
        for [product, name] in lines(products).skip(1).filter_map(fields::<2>) {
            let label = *labels.entry(name).or_insert_with(|| {
                let id = categories.len() as u32;
                categories.push(name);
                id
            });
            category.insert(product, label);
        }
        let mut campaign = FxHashMap::default();
        let mut names = vec![(b"organic".as_slice(), b"organic".as_slice())];
        for [id, channel] in lines(campaigns).skip(1).filter_map(fields::<2>) {
            campaign.insert(id, names.len() as u32);
            names.push((id, channel));
        }
        Self {
            category,
            categories,
            campaign,
            campaigns: names,
        }
    }
}

struct Window {
    start: i64,
    end: i64,
}

#[derive(Clone, Copy)]
struct Event<'a> {
    user: Name<'a>,
    ts: i64,
    price: u32,
    category: u32,
}

struct StagedEvent<'a> {
    id: Name<'a>,
    ingest: i64,
    event: Option<Event<'a>>,
}

struct Click<'a> {
    user: Name<'a>,
    ts: i64,
    id: Name<'a>,
    campaign: u32,
}

fn parse_event<'a>(
    row: &'a [u8],
    catalog: &Catalog,
    window: &Window,
) -> Option<StagedEvent<'a>> {
    let [id, user, ts, ingest, kind, product, price] = fields::<7>(row)?;
    if id.is_empty() {
        return None;
    }
    let ingest = fast_timestamp_f1(ingest)?;
    let event = (|| {
        if user.is_empty() || product.is_empty() {
            return None;
        }
        let ts = timestamp(ts)?;
        if ts < window.start || ts >= window.end || ingest - ts > DAY {
            return None;
        }
        let (price, category) = match kind {
            b"view" | b"cart" => (0, 0),
            b"purchase" => {
                let price = digits(price)?;
                if !(1..=10_000_000).contains(&price) {
                    return None;
                }
                (
                    price as u32,
                    catalog.category.get(product).copied().unwrap_or(0),
                )
            }
            _ => return None,
        };
        Some(Event {
            user,
            ts,
            price,
            category,
        })
    })();
    // An invalid earliest copy is still a deduplication winner.
    Some(StagedEvent { id, ingest, event })
}

fn parse_click<'a>(
    row: &'a [u8],
    people: &People<'a>,
    catalog: &Catalog,
    window: &Window,
) -> Option<Click<'a>> {
    let [id, user, campaign, ts] = fields::<4>(row)?;
    if id.is_empty() || user.is_empty() {
        return None;
    }
    let campaign = *catalog.campaign.get(campaign)?;
    let ts = timestamp(ts)?;
    (ts >= window.start - WEEK && ts < window.end).then(|| Click {
        user: people.name(user),
        ts,
        id,
        campaign,
    })
}

fn buckets<T, const N: usize>() -> [Vec<T>; N] {
    std::array::from_fn(|_| Vec::new())
}

fn publish<T, const N: usize>(local: [Vec<T>; N], shared: &[Mutex<Vec<T>>; N]) {
    for (batch, bucket) in local.into_iter().zip(shared) {
        if !batch.is_empty() {
            bucket.lock().unwrap().extend(batch);
        }
    }
}

fn collect_earliest_packed<'a>(
    mut entries: Vec<(u64, i64, Option<Event<'a>>)>,
    people_resolver: &People<'a>,
    people: &mut [Vec<Event<'a>>; BUCKETS],
) {
    if entries.is_empty() {
        return;
    }
    // Store row indexes so the full rows can stay in a compact sequential
    // array. Even the maximum dataset fits well below a u32 index per shard.
    let capacity = (entries.len() * 2).next_power_of_two();
    let mut slots = vec![0_u32; capacity];
    let shift = u64::BITS - capacity.trailing_zeros();
    for index in 0..entries.len() {
        let (key, ingest, _) = entries[index];
        let hash = key.rotate_left(23).wrapping_mul(0xd6e8_feb8_6659_fd93);
        let mut slot = (hash >> shift) as usize;
        loop {
            let previous = slots[slot];
            if previous == 0 {
                slots[slot] = (index + 1) as u32;
                break;
            }
            let previous = previous as usize - 1;
            if entries[previous].0 == key {
                if ingest < entries[previous].1 {
                    entries[previous].2 = None;
                    slots[slot] = (index + 1) as u32;
                } else {
                    entries[index].2 = None;
                }
                break;
            }
            slot = (slot + 1) & (capacity - 1);
        }
    }
    for mut event in entries.into_iter().filter_map(|(_, _, event)| event) {
        event.user = people_resolver.name(event.user);
        people[shard(event.user)].push(event);
    }
}

fn collect_earliest<'a, K: Eq + Hash>(
    entries: impl IntoIterator<Item = (K, i64, Option<Event<'a>>)>,
    count: usize,
    people_resolver: &People<'a>,
    people: &mut [Vec<Event<'a>>; BUCKETS],
) {
    let mut earliest = FxHashMap::with_capacity_and_hasher(count, Default::default());
    for (id, ingest, event) in entries {
        let kept = earliest.entry(id).or_insert((i64::MAX, None));
        if ingest < kept.0 {
            *kept = (ingest, event);
        }
    }
    for mut event in earliest.into_values().filter_map(|(_, event)| event) {
        event.user = people_resolver.name(event.user);
        people[shard(event.user)].push(event);
    }
}

type PackedEvent<'a> = (u64, i64, Option<Event<'a>>);

struct EventShards<'a> {
    packed: [Mutex<Vec<PackedEvent<'a>>>; EVENT_BUCKETS],
    other: [Mutex<Vec<StagedEvent<'a>>>; EVENT_BUCKETS],
}

fn stage_events<'a>(
    files: &'a [memmap2::Mmap],
    catalog: &Catalog,
    window: &Window,
) -> EventShards<'a> {
    let stages = EventShards {
        packed: std::array::from_fn(|_| Mutex::new(Vec::new())),
        other: std::array::from_fn(|_| Mutex::new(Vec::new())),
    };
    chunks(files).into_par_iter().for_each(|chunk| {
        let mut packed = buckets();
        let mut local = buckets();
        for event in lines(chunk).filter_map(|row| parse_event(row, catalog, window)) {
            if let [prefix, suffix @ ..] = event.id
                && let Ok(suffix) = <[u8; 8]>::try_from(suffix)
            {
                let key = u64::from_le_bytes(suffix);
                // The shard and suffix recover the prefix exactly, even if the
                // bytes aren't ASCII. The 256-way shard acts as the ninth byte.
                let bucket = (key.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 56) as u8 ^ prefix;
                packed[bucket as usize].push((key, event.ingest, event.event));
            } else {
                local[shard(event.id)].push(event);
            }
        }
        publish(packed, &stages.packed);
        publish(local, &stages.other);
    });
    stages
}

fn resolve_events<'a>(stages: EventShards<'a>, people: &People<'a>) -> Vec<Vec<Event<'a>>> {
    let people_buckets: [_; BUCKETS] = std::array::from_fn(|_| Mutex::new(Vec::new()));
    stages.packed
        .into_par_iter()
        .zip(stages.other)
        .for_each(|(packed, bucket)| {
            let packed = packed.into_inner().unwrap();
            let bucket = bucket.into_inner().unwrap();
            let mut local = buckets();
            collect_earliest_packed(packed, people, &mut local);
            let other_count = bucket.len();
            let other = bucket
                .into_iter()
                .map(|event| (event.id, event.ingest, event.event));
            collect_earliest(other, other_count, people, &mut local);
            publish(local, &people_buckets);
        });
    people_buckets
        .into_iter()
        .map(|part| part.into_inner().unwrap())
        .collect()
}

fn read_clicks<'a>(
    files: &'a [memmap2::Mmap],
    people: &People<'a>,
    catalog: &Catalog,
    window: &Window,
) -> Vec<Vec<Click<'a>>> {
    let shared: [_; BUCKETS] = std::array::from_fn(|_| Mutex::new(Vec::new()));
    chunks(files).into_par_iter().for_each(|chunk| {
        let mut local = buckets();
        for click in lines(chunk).filter_map(|row| parse_click(row, people, catalog, window)) {
            local[shard(click.user)].push(click);
        }
        publish(local, &shared);
    });
    // Valid copies are identical. Repeated clicks leave both the winner and the
    // set of active campaigns unchanged, so the rolling window absorbs them.
    shared
        .into_iter()
        .map(|part| part.into_inner().unwrap())
        .collect()
}

#[derive(Default)]
pub struct CampaignRow {
    pub sessions: i64,
    pub purchasing: i64,
    pub revenue: i64,
    pub assisted: i64,
}

pub struct UserRow<'a> {
    pub user: Name<'a>,
    pub day: i32,
    pub sessions: i64,
    pub events: i64,
    pub revenue: i64,
    pub category: Option<Name<'a>>,
}

type CampaignDays = FxHashMap<(u32, i32), CampaignRow>;

#[derive(Default)]
struct UserDay {
    sessions: i64,
    events: i64,
    revenue: i64,
    categories: FxHashMap<u32, i64>,
}

impl UserDay {
    fn take<'a>(&mut self, user: Name<'a>, day: i32, names: &[Name<'a>]) -> UserRow<'a> {
        let category = self
            .categories
            .drain()
            .map(|(id, cents)| (cents, std::cmp::Reverse(names[id as usize])))
            .max()
            .map(|(_, name)| name.0);
        UserRow {
            user,
            day,
            category,
            sessions: std::mem::take(&mut self.sessions),
            events: std::mem::take(&mut self.events),
            revenue: std::mem::take(&mut self.revenue),
        }
    }
}

struct Assists {
    active: Vec<u32>,
    counts: Vec<usize>,
    positions: Vec<usize>,
}

impl Assists {
    fn new(campaigns: usize) -> Self {
        Self {
            active: Vec::new(),
            counts: vec![0; campaigns],
            positions: vec![0; campaigns],
        }
    }

    fn add(&mut self, id: u32) {
        let i = id as usize;
        if self.counts[i] == 0 {
            self.positions[i] = self.active.len();
            self.active.push(id);
        }
        self.counts[i] += 1;
    }

    fn expire(&mut self, id: u32) {
        let i = id as usize;
        self.counts[i] -= 1;
        if self.counts[i] == 0 {
            let position = self.positions[i];
            self.active.swap_remove(position);
            if let Some(&moved) = self.active.get(position) {
                self.positions[moved as usize] = position;
            }
        }
    }

    fn clear(&mut self) {
        for id in self.active.drain(..) {
            self.counts[id as usize] = 0;
        }
    }
}

struct IndexedEvent {
    user: u32,
    ts: i64,
    price: u32,
    category: u32,
}

struct IndexedClick<'a> {
    user: u32,
    ts: i64,
    id: Name<'a>,
    campaign: u32,
}

fn aggregate<'a>(
    events: Vec<Event<'a>>,
    clicks: Vec<Click<'a>>,
    catalog: &Catalog<'a>,
) -> (Vec<UserRow<'a>>, CampaignDays) {
    let mut ids = FxHashMap::default();
    let mut names = Vec::new();
    let mut events: Vec<_> = events
        .into_iter()
        .map(|event| {
            let user = *ids.entry(event.user).or_insert_with(|| {
                let id = names.len() as u32;
                names.push(event.user);
                id
            });
            IndexedEvent {
                user,
                ts: event.ts,
                price: event.price,
                category: event.category,
            }
        })
        .collect();
    let mut clicks: Vec<_> = clicks
        .into_iter()
        .filter_map(|click| {
            let user = *ids.get(click.user)?;
            Some(IndexedClick {
                user,
                ts: click.ts,
                id: click.id,
                campaign: click.campaign,
            })
        })
        .collect();
    drop(ids);
    events.sort_unstable_by_key(|event| (event.user, event.ts));
    clicks.sort_unstable_by(|a, b| {
        a.user
            .cmp(&b.user)
            .then_with(|| a.ts.cmp(&b.ts))
            .then_with(|| b.id.cmp(a.id))
    });

    let mut campaign_days = CampaignDays::default();
    let mut user_days = Vec::new();
    let mut assists = Assists::new(catalog.campaigns.len());
    let mut totals = UserDay::default();
    let mut end_click = 0;
    for personal in events.chunk_by(|a, b| a.user == b.user) {
        let id = personal[0].user;
        let user = names[id as usize];
        while end_click < clicks.len() && clicks[end_click].user < id {
            end_click += 1;
        }
        let mut first = end_click;
        let mut next = end_click;
        while end_click < clicks.len() && clicks[end_click].user == id {
            end_click += 1;
        }
        let mut day = personal[0].ts.div_euclid(DAY) as i32;
        for session in personal.chunk_by(|a, b| b.ts - a.ts <= 1_800_000) {
            let start = session[0].ts;
            let session_day = start.div_euclid(DAY) as i32;
            if session_day != day {
                user_days.push(totals.take(user, day, &catalog.categories));
                day = session_day;
            }
            let mut revenue = 0;
            for event in session {
                if event.price > 0 {
                    let price = i64::from(event.price);
                    revenue += price;
                    *totals.categories.entry(event.category).or_default() += price;
                }
            }
            totals.sessions += 1;
            totals.events += session.len() as i64;
            totals.revenue += revenue;

            while next < end_click && clicks[next].ts <= start {
                assists.add(clicks[next].campaign);
                next += 1;
            }
            while first < next && clicks[first].ts < start - WEEK {
                assists.expire(clicks[first].campaign);
                first += 1;
            }
            let winner = if first < next {
                clicks[next - 1].campaign
            } else {
                0
            };
            let credited = campaign_days.entry((winner, day)).or_default();
            credited.sessions += 1;
            credited.purchasing += i64::from(revenue > 0);
            credited.revenue += revenue;
            for campaign in &assists.active {
                campaign_days.entry((*campaign, day)).or_default().assisted += 1;
            }
        }
        user_days.push(totals.take(user, day, &catalog.categories));
        assists.clear();
    }
    (user_days, campaign_days)
}

fn merge_campaigns(mut result: CampaignDays, other: CampaignDays) -> CampaignDays {
    for (key, value) in other {
        let row = result.entry(key).or_default();
        row.sessions += value.sessions;
        row.purchasing += value.purchasing;
        row.revenue += value.revenue;
        row.assisted += value.assisted;
    }
    result
}

fn setting(key: &str, position: usize, default: &str) -> PathBuf {
    std::env::var_os(key)
        .or_else(|| std::env::args_os().nth(position))
        .map_or_else(|| PathBuf::from(default), PathBuf::from)
}

fn run(data: &Path, out: &Path) {
    let start = Instant::now();
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join("manifest.json")).unwrap()).unwrap();
    let window = Window {
        start: timestamp(document["window_start"].as_str().unwrap().as_bytes()).unwrap(),
        end: timestamp(document["window_end"].as_str().unwrap().as_bytes()).unwrap(),
    };
    let products = fs::read(data.join("products.csv")).unwrap();
    let campaigns = fs::read(data.join("campaigns.csv")).unwrap();
    let catalog = Catalog::new(&products, &campaigns);
    let links = maps(&data.join("links"));
    let event_files = maps(&data.join("events"));
    let click_files = maps(&data.join("clicks"));
    let t_maps = start.elapsed();
    // Event-ID deduplication does not depend on the canonical user names.
    let (people, staged) = rayon::join(
        || People::from_links(&links),
        || stage_events(&event_files, &catalog, &window),
    );
    let t_staged = start.elapsed();
    let (events, clicks) = rayon::join(
        || resolve_events(staged, &people),
        || read_clicks(&click_files, &people, &catalog, &window),
    );
    let t_resolved = start.elapsed();

    let user_out = out.join("user_daily");
    let campaign_out = out.join("campaign_daily");
    fs::create_dir_all(&user_out).unwrap();
    fs::create_dir_all(&campaign_out).unwrap();
    let totals = events
        .into_par_iter()
        .zip(clicks)
        .enumerate()
        .map(|(part, (events, clicks))| {
            let (users, totals) = aggregate(events, clicks, &catalog);
            if part == 0 || !users.is_empty() {
                output::users(&user_out.join(format!("part-{part:03}.parquet")), &users);
            }
            totals
        })
        .reduce(CampaignDays::default, merge_campaigns);
    output::campaigns(
        &campaign_out.join("part-000.parquet"),
        totals,
        &catalog.campaigns,
    );
    let t_total = start.elapsed();
    eprintln!(
        "TIMING: setup={:.3}s links_and_event_parse={:.3}s event_dedup_and_clicks={:.3}s output={:.3}s TOTAL={:.3}s",
        t_maps.as_secs_f64(),
        (t_staged - t_maps).as_secs_f64(),
        (t_resolved - t_staged).as_secs_f64(),
        (t_total - t_resolved).as_secs_f64(),
        t_total.as_secs_f64()
    );
}

fn main() {
    let threads = std::env::var("NCPU")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(8, |count| count.get().min(8))
        });
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .unwrap();
    run(
        &setting("DATA_DIR", 1, "/data"),
        &setting("OUTPUT_DIR", 2, "/output"),
    );
}
