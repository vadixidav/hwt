use chrono::Utc;
use hwt::*;
use log::LevelFilter;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use std::path::PathBuf;

#[test]
fn test_neighbors() {
    let features = [
        0b1001,
        0b1010,
        0b1100,
        0b1000,
        0xAAAA_AAAA_AAAA_AAAA_AAAA_AAAA_AAAA_AAAA,
    ];
    let mut hwt = Hwt::new();
    for &feature in &features {
        hwt.insert(feature);
    }

    for &feature in &features {
        let mut neighbors = [0; 1];
        let neighbors = hwt.nearest(feature, 128, &mut neighbors);
        assert_eq!(neighbors[0], feature);
    }

    let mut neighbors = hwt.search_radius(1, 0b1000).collect::<Vec<u128>>();
    neighbors.sort_unstable();
    assert_eq!(&neighbors, &[0b1000, 0b1001, 0b1010, 0b1100]);

    let mut neighbors = hwt.search_radius(1, 0b1001).collect::<Vec<u128>>();
    neighbors.sort_unstable();
    assert_eq!(&neighbors, &[0b1000, 0b1001]);

    let mut neighbors = hwt.search_radius(1, 0b1010).collect::<Vec<u128>>();
    neighbors.sort_unstable();
    assert_eq!(&neighbors, &[0b1000, 0b1010]);

    let mut neighbors = hwt.search_radius(1, 0b1100).collect::<Vec<u128>>();
    neighbors.sort_unstable();
    assert_eq!(&neighbors, &[0b1000, 0b1100]);

    let range = (0..).take(1 << 4);
    let mut hwt = Hwt::new();
    for i in range.clone() {
        hwt.insert(i);
    }
    for feature in range.clone() {
        assert!(hwt.search_radius(2, feature).count() < 8128);
    }
}

#[test]
fn compare_to_linear() -> std::io::Result<()> {
    // Start logging.
    let now = Utc::now();
    let log_dir = PathBuf::from("target").join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let log_file = log_dir.join(now.format("%Z_%F_%H-%M-%S.txt").to_string());
    eprintln!("logging in {}", log_file.display());
    simple_logging::log_to_file(&log_file, LevelFilter::Trace)?;

    let mut rng = SmallRng::from_seed([5; 16]);
    let space = rng
        .sample_iter(&rand::distributions::Standard)
        .take(100_000)
        .collect::<Vec<u128>>();
    let search = rng
        .sample_iter(&rand::distributions::Standard)
        .take(10)
        .collect::<Vec<u128>>();

    let mut hwt = Hwt::new();
    for &f in &space {
        hwt.insert(f);
    }

    for f0 in search {
        let mut neighbors = [0; 1];
        let neighbors = hwt.nearest(f0, 128, &mut neighbors);
        assert_eq!(
            space
                .iter()
                .map(|&f1| (f0 ^ f1).count_ones())
                .min()
                .unwrap(),
            (neighbors[0] ^ f0).count_ones()
        );
    }

    Ok(())
}
