use crate::indices::*;
use crate::search::*;
use crate::NodeQueue;
use hashbrown::{hash_map::Entry, HashMap};
use log::trace;
use std::cmp::{max, min};
use swar::*;

/// This threshold determines whether to perform a brute-force search in a bucket
/// instead of a targeted search if the amount of nodes is less than this number.
///
/// Since we do a brute force search in an internal node with < `TAU` leaves,
/// this also defines the threshold at which a vector must be split into a hash table.
///
/// This should be improved by changing the threshold on a per-level of the tree basis.
const TAU: usize = 1024;

const MAP_TAUS: [usize; 7] = [TAU, TAU, TAU, TAU, TAU, TAU, TAU];

/// This determines how much space is initially allocated for a leaf vector.
const INITIAL_CAPACITY: usize = 4;

#[derive(Debug)]
enum Internal {
    /// This always contains features.
    Vec(Vec<u128>),
    /// This always points to another internal node.
    Map(HashMap<u128, u32, std::hash::BuildHasherDefault<ahash::AHasher>>),
}

impl Default for Internal {
    fn default() -> Self {
        Internal::Vec(Vec::with_capacity(INITIAL_CAPACITY))
    }
}

pub struct Hwt {
    /// A `u32` pointing to an internal node is just an index into the
    /// internals array, which is just a bump allocator for internal nodes.
    internals: Vec<Internal>,
    count: usize,
}

impl Hwt {
    /// Makes an empty `Hwt`.
    ///
    /// ```
    /// # use hwt::Hwt;
    /// let hwt = Hwt::new();
    /// assert!(hwt.is_empty());
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Gets the number of entries in the `Hwt`.
    ///
    /// ```
    /// # use hwt::Hwt;
    /// let mut hwt = Hwt::new();
    /// hwt.insert(0b101);
    /// assert_eq!(hwt.len(), 1);
    /// ```
    pub fn len(&self) -> usize {
        self.count
    }

    /// Checks if the `Hwt` is empty.
    ///
    /// ```
    /// # use hwt::Hwt;
    /// let mut hwt = Hwt::new();
    /// assert!(hwt.is_empty());
    /// hwt.insert(0b101);
    /// assert!(!hwt.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn allocate_internal(&mut self) -> u32 {
        let internal = self.internals.len() as u32;
        assert!(internal < std::u32::MAX);
        self.internals.push(Internal::default());
        internal
    }

    /// Converts an internal node from a `Vec` of leaves to a `HashMap` from indices to internal nodes.
    ///
    /// `internal` must be the internal node index which should be replaced
    /// `level` must be set from 0 to 7 inclusive. If it is 0, this is the root.
    fn convert(&mut self, internal: usize, level: usize) {
        // Swap a temporary vec with the one in the store to avoid the wrath of the borrow checker.
        let mut old_vec = Internal::Vec(Vec::new());
        std::mem::swap(&mut self.internals[internal], &mut old_vec);
        // Use the old vec to create a new map for the node.
        self.internals[internal] = match old_vec {
            Internal::Vec(v) => {
                let mut map = HashMap::default();
                for feature in v.into_iter() {
                    let index = indices128(feature)[level];
                    let new_internal =
                        *map.entry(index).or_insert_with(|| self.allocate_internal());
                    if let Internal::Vec(ref mut v) = self.internals[new_internal as usize] {
                        v.push(feature);
                    } else {
                        unreachable!(
                            "cannot have InternalStore::Map in subtable when just created"
                        );
                    }
                }
                Internal::Map(map)
            }
            _ => panic!("tried to convert an InternalStore::Map"),
        }
    }

    /// Inserts an item ID to the `Hwt`.
    ///
    /// - `F`: A function which should give the `feature` for the given ID.
    ///
    /// The most significant bit must not be set on the `item`.
    ///
    /// Returns `Some(t)` if item `t` was replaced by `item`.
    ///
    /// ```
    /// # use hwt::Hwt;
    /// let mut hwt = Hwt::new();
    /// hwt.insert(0b101);
    /// hwt.insert(0b010);
    /// assert_eq!(hwt.len(), 2);
    /// ```
    pub fn insert(&mut self, feature: u128) {
        // No matter what we will insert the item, so increase the count now.
        self.count += 1;
        // Compute the indices of the buckets and the sizes of the buckets
        // for each layer of the tree.
        let indices = indices128(feature);
        let mut bucket = 0;
        let mut create_internal = None;
        for (i, &node) in indices.iter().enumerate() {
            match &mut self.internals[bucket] {
                Internal::Vec(ref mut v) => {
                    v.push(feature);
                    if v.len() > TAU {
                        self.convert(bucket, i);
                    }
                    return;
                }
                Internal::Map(ref mut map) => {
                    match map.entry(node) {
                        Entry::Occupied(o) => {
                            let internal = *o.get();
                            // Go to the next node.
                            bucket = internal as usize;
                        }
                        Entry::Vacant(_) => {
                            create_internal = Some(node);
                            break;
                        }
                    }
                }
            }
        }
        if let Some(vacant_node) = create_internal {
            // Allocate a new internal Vec node.
            let new_internal = self.allocate_internal();
            // Add the item to the new internal Vec.
            if let Internal::Vec(ref mut v) = self.internals[new_internal as usize] {
                v.push(feature);
            } else {
                unreachable!("cannot have InternalStore::Map in subtable when just created");
            }
            // Add the new internal to the vacant map spot.
            if let Internal::Map(ref mut map) = &mut self.internals[bucket] {
                map.insert(vacant_node, new_internal);
            } else {
                unreachable!("shouldn't ever get vec after finding vacant map node");
            }
        } else {
            // We are just adding this item to the bottom of the tree in a Vec.
            match self.internals[bucket] {
                Internal::Vec(ref mut v) => v.push(feature),
                _ => panic!("Can't have InternalStore::Map at bottom of tree"),
            }
        }
    }

    /// Checks if a feature is in the `Hwt`.
    ///
    /// ```
    /// # use hwt::Hwt;
    /// let mut hwt = Hwt::new();
    /// hwt.insert(0b101);
    /// hwt.insert(0b010);
    /// assert!(hwt.contains(0b101));
    /// assert!(hwt.contains(0b010));
    /// assert!(!hwt.contains(0b000));
    /// assert!(!hwt.contains(0b111));
    /// ```
    pub fn contains(&mut self, feature: u128) -> bool {
        // Compute the indices of the buckets and the sizes of the buckets
        // for each layer of the tree.
        let indices = indices128(feature);
        let mut bucket = 0;
        for index in &indices {
            match &self.internals[bucket] {
                Internal::Vec(vec) => return vec.iter().cloned().any(|n| n == feature),
                Internal::Map(map) => {
                    if let Some(&occupied_node) = map.get(index) {
                        bucket = occupied_node as usize;
                    } else {
                        return false;
                    }
                }
            }
        }
        false
    }

    /// Find the nearest neighbors to a feature. This will give the nearest
    /// neighbors first and expand outwards. It will fill `dest` until its full
    /// with nearest neighbors in order. `lookup` must be able to retrieve the
    /// feature for a given leaf index.
    ///
    /// Returns the slice of filled neighbors. It may not consume all of `dest`.
    pub fn nearest<'a>(&self, feature: u128, dest: &'a mut [u128]) -> &'a mut [u128] {
        trace!(
            "nearest feature({:032X}) weight({})",
            feature,
            feature.count_ones()
        );
        let destlen = dest.len();
        let indices = indices128(feature);
        let (mut next, mut remaining) = match dest.split_first_mut() {
            Some(n) => n,
            None => return dest,
        };
        let lookup_distance = |leaf: u128| (leaf ^ feature).count_ones();
        // Expand the root node.
        let mut node_queue = NodeQueue::new(match &self.internals[0] {
            Internal::Vec(v) => {
                trace!("nearest sole leaf node len({})", v.len());
                let mut v = v.clone();
                // TODO: Benchmark sorting by cached key as well since
                // the performance of a small hamming weight tree is actually
                // quite relevant in many scenarios (matching two views).
                v.sort_unstable_by_key(|&a| lookup_distance(a));
                let final_len = std::cmp::min(destlen, v.len());
                for (d, s) in dest.iter_mut().zip(v) {
                    *d = s;
                }
                // This was the whole thing (returning here is not necessary, but faster).
                return &mut dest[0..final_len];
            }
            Internal::Map(m) => {
                trace!("nearest emptying root len({})", m.len());
                m.iter().map(|(&tc, &node)| {
                    let distance = (tc as i32 - indices[0] as i32).abs() as u32;
                    (distance, tc, node)
                })
            }
        });

        while let Some((distance, tp, node, level)) = node_queue.pop() {
            match &self.internals[node as usize] {
                Internal::Vec(v) => {
                    trace!(
                        "nearest leaf vec tp({:032X}) distance({}) len({}) level({})",
                        tp,
                        distance,
                        v.len(),
                        level
                    );
                    // We will accumulate the minimum leaf distance over `distance`
                    // into this variable so we know when to search this leaf again.
                    let mut min_over_distance = 129;
                    for leaf in v.iter().cloned().filter(|&other| {
                        let leaf_distance = lookup_distance(other);
                        if leaf_distance < min_over_distance && leaf_distance > distance {
                            min_over_distance = leaf_distance;
                        }
                        leaf_distance == distance
                    }) {
                        *next = leaf;
                        match remaining.split_first_mut() {
                            Some((new_next, new_remaining)) => {
                                next = new_next;
                                remaining = new_remaining;
                            }
                            None => return dest,
                        };
                    }
                    // If we found a distance in the valid range.
                    if min_over_distance < 129 {
                        // Re-add the leaf node with a higher distance so we revisit it at that time.
                        node_queue.add_one((min_over_distance, tp, node, level));
                    }
                }
                Internal::Map(m) => {
                    if level == 7 {
                        unreachable!("hwt: it is impossible to have an internal node at layer 7");
                    }
                    if m.len() < MAP_TAUS[level as usize] {
                        trace!(
                            "nearest brute force tp({:032X}) distance({}) len({}) level({})",
                            tp,
                            distance,
                            m.len(),
                            level
                        );
                        node_queue.add(m.iter().map(|(&tc, &child)| {
                            let child_distance = index_distance(tc, &indices, level + 1);
                            (child_distance, tc, child, level + 1)
                        }));
                    } else {
                        trace!(
                            "nearest precision search tp({:032X}) distance({}) len({}) level({})",
                            tp,
                            distance,
                            m.len(),
                            level
                        );
                        let filter_map =
                            |tc| m.get(&tc).map(|&child| (distance, tc, child, level + 1));
                        match level {
                            0 => node_queue.add(
                                search_exact2(
                                    Bits128(indices[0]),
                                    Bits64(indices[1]),
                                    Bits128(tp),
                                    distance,
                                )
                                .map(|n| n.0)
                                .filter_map(filter_map),
                            ),
                            1 => node_queue.add(
                                search_exact4(
                                    Bits64(indices[1]),
                                    Bits32(indices[2]),
                                    Bits64(tp),
                                    distance,
                                )
                                .map(|n| n.0)
                                .filter_map(filter_map),
                            ),
                            2 => node_queue.add(
                                search_exact8(
                                    Bits32(indices[2]),
                                    Bits16(indices[3]),
                                    Bits32(tp),
                                    distance,
                                )
                                .map(|n| n.0)
                                .filter_map(filter_map),
                            ),
                            3 => node_queue.add(
                                search_exact16(
                                    Bits16(indices[3]),
                                    Bits8(indices[4]),
                                    Bits16(tp),
                                    distance,
                                )
                                .map(|n| n.0)
                                .filter_map(filter_map),
                            ),
                            4 => node_queue.add(
                                search_exact32(
                                    Bits8(indices[4]),
                                    Bits4(indices[5]),
                                    Bits8(tp),
                                    distance,
                                )
                                .map(|n| n.0)
                                .filter_map(filter_map),
                            ),
                            5 => node_queue.add(
                                search_exact64(
                                    Bits4(indices[5]),
                                    Bits2(indices[6]),
                                    Bits4(tp),
                                    distance,
                                )
                                .map(|n| n.0)
                                .filter_map(filter_map),
                            ),
                            6 => node_queue.add(
                                search_exact128(
                                    Bits2(indices[6]),
                                    Bits1(indices[7]),
                                    Bits2(tp),
                                    distance,
                                )
                                .map(|n| n.0)
                                .filter_map(filter_map),
                            ),
                            _ => unreachable!(),
                        }
                        if distance != 128 {
                            // Re-add the node at a higher distance since we already got everything
                            // from `distance - 1`.
                            node_queue.add_one((distance + 1, tp, node, level));
                        }
                    }
                }
            }
        }
        let remlen = remaining.len();
        &mut dest[0..destlen - remlen]
    }

    /// Find all neighbors within a given radius.
    pub fn search_radius<'a>(
        &'a self,
        radius: u32,
        feature: u128,
    ) -> impl Iterator<Item = u128> + 'a {
        let indices = indices128(feature);
        let sw = indices[0] as i32;
        let start = max(0, sw - radius as i32) as u128;
        let end = min(128, sw + radius as i32) as u128;
        // Iterate over every applicable index in the root.
        self.bucket_scan_radius(
            radius,
            feature,
            0, // The index is the `tw` because at the root node indices
            // are target weights.
            start..=end,
            Self::radius2,
            move |tc| Bits64(tc).hwd(Bits64(indices[1])).sum_weight() as u32 <= radius,
        )
    }

    fn radius2<'a>(
        &'a self,
        radius: u32,
        feature: u128,
        bucket: usize,
        tp: u128,
    ) -> impl Iterator<Item = u128> + 'a {
        let indices = indices128(feature);
        self.bucket_scan_radius(
            radius,
            feature,
            bucket,
            search_radius2(Bits128(indices[0]), Bits64(indices[1]), Bits128(tp), radius)
                .map(|(tc, _sod)| tc.0),
            Self::radius4,
            move |tc| Bits32(tc).hwd(Bits32(indices[2])).sum_weight() as u32 <= radius,
        )
    }

    fn radius4<'a>(
        &'a self,
        radius: u32,
        feature: u128,
        bucket: usize,
        tp: u128,
    ) -> impl Iterator<Item = u128> + 'a {
        let indices = indices128(feature);
        self.bucket_scan_radius(
            radius,
            feature,
            bucket,
            search_radius4(Bits64(indices[1]), Bits32(indices[2]), Bits64(tp), radius)
                .map(|(tc, _sod)| tc.0),
            Self::radius8,
            move |tc| Bits16(tc).hwd(Bits16(indices[3])).sum_weight() as u32 <= radius,
        )
    }

    fn radius8<'a>(
        &'a self,
        radius: u32,
        feature: u128,
        bucket: usize,
        tp: u128,
    ) -> impl Iterator<Item = u128> + 'a {
        let indices = indices128(feature);
        self.bucket_scan_radius(
            radius,
            feature,
            bucket,
            search_radius8(Bits32(indices[2]), Bits16(indices[3]), Bits32(tp), radius)
                .map(|(tc, _sod)| tc.0),
            Self::radius16,
            move |tc| Bits8(tc).hwd(Bits8(indices[4])).sum_weight() as u32 <= radius,
        )
    }

    fn radius16<'a>(
        &'a self,
        radius: u32,
        feature: u128,
        bucket: usize,
        tp: u128,
    ) -> impl Iterator<Item = u128> + 'a {
        let indices = indices128(feature);
        self.bucket_scan_radius(
            radius,
            feature,
            bucket,
            search_radius16(Bits16(indices[3]), Bits8(indices[4]), Bits16(tp), radius)
                .map(|(tc, _sod)| tc.0),
            Self::radius32,
            move |tc| Bits4(tc).hwd(Bits4(indices[5])).sum_weight() as u32 <= radius,
        )
    }

    fn radius32<'a>(
        &'a self,
        radius: u32,
        feature: u128,
        bucket: usize,
        tp: u128,
    ) -> impl Iterator<Item = u128> + 'a {
        let indices = indices128(feature);
        self.bucket_scan_radius(
            radius,
            feature,
            bucket,
            search_radius32(Bits8(indices[4]), Bits4(indices[5]), Bits8(tp), radius)
                .map(|(tc, _sod)| tc.0),
            Self::radius64,
            move |tc| Bits2(tc).hwd(Bits2(indices[6])).sum_weight() as u32 <= radius,
        )
    }

    fn radius64<'a>(
        &'a self,
        radius: u32,
        feature: u128,
        bucket: usize,
        tp: u128,
    ) -> impl Iterator<Item = u128> + 'a {
        let indices = indices128(feature);
        self.bucket_scan_radius(
            radius,
            feature,
            bucket,
            search_radius64(Bits4(indices[5]), Bits2(indices[6]), Bits4(tp), radius)
                .map(|(tc, _sod)| tc.0),
            Self::radius128,
            move |tc| Bits1(tc).hwd(Bits1(indices[7])).sum_weight() as u32 <= radius,
        )
    }

    fn radius128<'a>(
        &'a self,
        radius: u32,
        feature: u128,
        bucket: usize,
        tp: u128,
    ) -> impl Iterator<Item = u128> + 'a {
        let indices = indices128(feature);
        self.bucket_scan_radius(
            radius,
            feature,
            bucket,
            search_radius128(Bits2(indices[6]), Bits1(indices[7]), Bits2(tp), radius).map(|(tc, _sod)| tc.0),
            |_, _, _, bucket, _| -> Box<dyn Iterator<Item = u128> + 'a> {
                panic!(
                    "hwt::Hwt::neighbors128(): it is an error to find an internal node this far down in the tree (bucket: {})", bucket, 
                )
            },
            move |tc| panic!("hwt::Hwt::neighbors128(): it is an error to find an internal node this far down in the tree (tc: {})", tc)
        )
    }

    /// Search the given `bucket` with the `indices` iterator, using `subtable`
    /// to recursively iterate over buckets found inside this bucket.
    #[allow(clippy::too_many_arguments)]
    fn bucket_scan_radius<'a, I: 'a>(
        &'a self,
        radius: u32,
        feature: u128,
        bucket: usize,
        indices: impl Iterator<Item = u128> + 'a,
        subtable: fn(&'a Self, u32, u128, usize, u128) -> I,
        filter: impl Fn(u128) -> bool + 'a,
    ) -> Box<dyn Iterator<Item = u128> + 'a>
    where
        I: Iterator<Item = u128>,
    {
        trace!(
            "bucket_scan_radius feature({:032X}) radius({}) bucket({})",
            feature,
            radius,
            bucket,
        );
        let lookup_distance = move |leaf: u128| (leaf ^ feature).count_ones();
        match &self.internals[bucket] {
            Internal::Vec(v) => Box::new(
                v.iter()
                    .cloned()
                    .filter(move |&leaf| lookup_distance(leaf) <= radius),
            ),
            Internal::Map(m) => {
                if m.len() < TAU {
                    Box::new(m.iter().filter(move |&(&key, _)| filter(key)).flat_map(
                        move |(&tc, &node)| subtable(self, radius, feature, node as usize, tc),
                    ))
                } else {
                    Box::new(
                        indices
                            .filter_map(move |tc| m.get(&tc).map(|&node| (tc, node)))
                            .flat_map(move |(tc, node)| {
                                subtable(self, radius, feature, node as usize, tc)
                            }),
                    )
                }
            }
        }
    }
}

impl Default for Hwt {
    fn default() -> Self {
        // The number of child nodes of the root is determined by the different
        // possible hamming weights. The maximum hamming weight is the number
        // of bits and the minimum is 0, so this means that there are
        // `NBits + 1` child nodes.
        Self {
            internals: vec![Internal::default()],
            count: 0,
        }
    }
}
