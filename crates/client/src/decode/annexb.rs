//! Splitting Annex-B byte streams (H.264 and HEVC use the same framing) into NAL units.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

/// Returns each NAL unit without its start code. Handles 3 and 4 byte start codes.
pub fn split(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            let end = match starts.get(n + 1) {
                // Back up over the next start code, and the extra zero of a 4 byte one.
                Some(&next) => {
                    let mut end = next - 3;
                    if end > start && data[end - 1] == 0 {
                        end -= 1;
                    }
                    end
                }
                None => data.len(),
            };
            &data[start..end]
        })
        .filter(|nal| !nal.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_mixed_start_codes() {
        let data = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        let nals = split(&data);
        assert_eq!(nals, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
    }
}
