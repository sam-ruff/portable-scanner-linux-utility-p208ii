//! Maths for turning calibration scans into analogue and per pixel corrections.
//! The targets match the SANE `canon_dr` backend, which was tuned on hardware.

pub const START_EXPOSURE: u16 = 0x320;

/// Black level from a lamp-off scan.
pub fn afe_offset(first_line: &[u8]) -> u8 {
    let min = first_line.iter().copied().min().unwrap_or(0);
    // Wraps like the reference driver when the black level is already zero
    (i32::from(min) * 3 - 2) as u8
}

pub fn channel_max(first_line: &[u8], channel: usize) -> u8 {
    first_line
        .iter()
        .skip(channel)
        .step_by(3)
        .copied()
        .max()
        .unwrap_or(0)
}

/// Scales exposure so the brightest pixel lands on a fixed target.
pub fn afe_exposure(current: u16, brightest: u8, colour: bool) -> u16 {
    if brightest == 0 {
        return current;
    }
    let target = if colour { 102 } else { 64 };
    let scaled = u32::from(current) * target / u32::from(brightest);
    scaled.min(u32::from(u16::MAX)) as u16
}

pub fn afe_gain(brightest: u8, colour: bool) -> u8 {
    let target = if colour { 250 } else { 125 };
    let gain = (target - i32::from(brightest)) * 4 / 5;
    gain.clamp(1, 255) as u8
}

/// Averages each byte position down the given number of lines.
pub fn column_average(data: &[u8], bytes_per_line: usize, lines: usize) -> Vec<u8> {
    let lines = lines.min(data.len() / bytes_per_line.max(1));
    if lines == 0 {
        return vec![0; bytes_per_line];
    }
    (0..bytes_per_line)
        .map(|column| {
            let sum: usize = (0..lines)
                .map(|line| usize::from(data[line * bytes_per_line + column]))
                .sum();
            (sum / lines) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_is_three_times_black_level() {
        assert_eq!(afe_offset(&[40, 12, 30]), 34);
        assert_eq!(afe_offset(&[1, 5]), 1);
    }

    #[test]
    fn offset_wraps_at_zero_like_reference_driver() {
        assert_eq!(afe_offset(&[0, 9]), 254);
    }

    #[test]
    fn channel_max_reads_interleaved_rgb() {
        let line = [1, 50, 3, 4, 5, 90];
        assert_eq!(channel_max(&line, 0), 4);
        assert_eq!(channel_max(&line, 1), 50);
        assert_eq!(channel_max(&line, 2), 90);
    }

    #[test]
    fn exposure_targets_depend_on_mode() {
        assert_eq!(afe_exposure(800, 204, true), 400);
        assert_eq!(afe_exposure(800, 128, false), 400);
    }

    #[test]
    fn exposure_survives_dark_readings() {
        assert_eq!(afe_exposure(800, 0, true), 800);
        assert_eq!(afe_exposure(800, 1, true), u16::MAX);
    }

    #[test]
    fn gain_is_at_least_one() {
        assert_eq!(afe_gain(200, true), 40);
        assert_eq!(afe_gain(255, true), 1);
        assert_eq!(afe_gain(100, false), 20);
    }

    #[test]
    fn column_average_uses_each_line() {
        let data = [10, 20, 30, 40];
        assert_eq!(column_average(&data, 2, 2), vec![20, 30]);
    }

    #[test]
    fn column_average_limits_to_available_lines() {
        let data = [10, 20];
        assert_eq!(column_average(&data, 2, 8), vec![10, 20]);
        assert_eq!(column_average(&[], 2, 8), vec![0, 0]);
    }
}
