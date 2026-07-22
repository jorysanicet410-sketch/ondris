/// Cible maximale (= difficulté 1) : 2^256 - 1.
pub const MAX_TARGET: [u8; 32] = [0xff; 32];

/// Convertit une difficulté (nombre simple, pas de format compact type
/// Bitcoin nBits — volontairement plus simple pour limiter les risques de
/// bug dans une implémentation de référence) en cible 256 bits big-endian :
/// `target = MAX_TARGET / difficulty`.
pub fn target_for_difficulty(difficulty: u64) -> [u8; 32] {
    let difficulty = difficulty.max(1);
    divide_be_by_u64(MAX_TARGET, difficulty)
}

fn divide_be_by_u64(num: [u8; 32], divisor: u64) -> [u8; 32] {
    let mut remainder: u128 = 0;
    let mut result = [0u8; 32];
    for i in 0..32 {
        let cur = (remainder << 8) | num[i] as u128;
        result[i] = (cur / divisor as u128) as u8;
        remainder = cur % divisor as u128;
    }
    result
}

/// Réajuste la difficulté en fonction du temps réellement observé sur la
/// fenêtre de blocs par rapport au temps cible, avec un facteur de
/// correction borné à [1/4x, 4x] pour éviter les à-coups violents.
pub fn next_difficulty(
    prev_difficulty: u64,
    actual_timespan_secs: u64,
    target_block_time_secs: u64,
    window: u64,
) -> u64 {
    let target_timespan = target_block_time_secs.saturating_mul(window).max(1);
    let actual = actual_timespan_secs
        .max(target_timespan / 4)
        .min(target_timespan * 4)
        .max(1);
    let new_diff = (prev_difficulty as u128 * target_timespan as u128 / actual as u128) as u64;
    new_diff.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn difficulty_one_gives_max_target() {
        assert_eq!(target_for_difficulty(1), MAX_TARGET);
    }

    #[test]
    fn higher_difficulty_gives_smaller_target() {
        let t1 = target_for_difficulty(1000);
        let t2 = target_for_difficulty(2000);
        assert!(t2 < t1);
    }

    #[test]
    fn faster_blocks_increase_difficulty() {
        // Les blocs sont arrivés plus vite que prévu -> la difficulté doit monter.
        let d = next_difficulty(1000, 15, 30, 60); // 60 blocs en 15s au lieu de 30s visé... trop vite
        assert!(d > 1000);
    }

    #[test]
    fn slower_blocks_decrease_difficulty() {
        let d = next_difficulty(1000, 60 * 60 * 2, 30, 60);
        assert!(d < 1000);
    }
}
