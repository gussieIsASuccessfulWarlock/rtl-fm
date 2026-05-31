//! NRSC-5 numeric constants for FM Hybrid (MP1) mode.
//!
//! Values ported (not copied) from the theori/nrsc5 C reference:
//!   src/defines.h, src/sync.h, src/decode.c.
//! `theori/nrsc5` is GPLv3. We re-implement the algorithms in fresh
//! Rust; only well-known protocol constants (FFT sizes, generator
//! polynomials, partition geometry) are reused, which are not
//! copyrightable in themselves.

use std::sync::LazyLock;

/// RTL-SDR bulk IQ rate.
pub const RTL_IQ_RATE: f32 = 2_400_000.0;
/// RTL-SDR sample rate for dedicated NRSC-5 decode (matches C nrsc5 NRSC5_SAMPLE_RATE_CU8).
/// At this rate the RTL-SDR is tuned directly to the target station center; the
/// NRSC-5 pipeline decimates by 2 to reach NRSC5_IQ_RATE = 744 187.5 S/s.
pub const NRSC5_RTL_RATE: u32 = 1_488_375;
/// NRSC-5 processing rate. 2.4 MS/s * 3969 / 12800 = 744_187.5 Hz.
pub const NRSC5_IQ_RATE: f32 = 744_187.5;
pub const RESAMPLE_INTERP: usize = 3969;
pub const RESAMPLE_DECIM: usize = 12800;

/// OFDM framing (FM mode). See defines.h:12-20.
pub const FFT_FM: usize = 2048;
pub const CP_FM: usize = 112;
pub const FFTCP_FM: usize = FFT_FM + CP_FM;
/// OFDM symbols per L1 block (`BLKSZ`).
pub const BLKSZ: usize = 32;

/// Sideband edges as FFT bin indices (DC-centered → fftshifted to
/// FFT_FM/2). defines.h:24-26.
pub const LB_START: usize = (FFT_FM / 2) - 546; // 478
pub const UB_END: usize = (FFT_FM / 2) + 546; // 1570

/// Partition layout. defines.h:75-79.
pub const PARTITION_WIDTH_FM: usize = 19; // 1 reference + 18 data
pub const PARTITION_DATA_CARRIERS: usize = 18;
pub const PM_PARTITIONS: usize = 10;

/// Primary Main interleaver block size. defines.h:81.
pub const PM_BLOCK_SIZE: usize = 2 * 2 * PM_PARTITIONS * PARTITION_DATA_CARRIERS * BLKSZ; // 23_040

/// Lower/upper sideband each contribute (PM_PARTITIONS + 1) references.
pub const NUM_REF_FM: usize = 2 * (PM_PARTITIONS + 1); // 22

/// Used to derive each partition's reference-subcarrier ID. sync.h:27.
pub const MIDDLE_REF_SC: i32 = 30;

/// FFT bin indices of all 22 Primary Main reference subcarriers.
/// Order: lower sideband i=0..10 (outer-to-inner), then upper i=0..10.
pub static REF_BINS_FM: LazyLock<Vec<usize>> = LazyLock::new(|| {
    let mut v = Vec::with_capacity(NUM_REF_FM);
    for i in 0..=PM_PARTITIONS {
        v.push(LB_START + i * PARTITION_WIDTH_FM);
    }
    for i in 0..=PM_PARTITIONS {
        v.push(UB_END - i * PARTITION_WIDTH_FM);
    }
    v
});

/// FFT bin indices of every P1 data carrier (360 total = 10 partitions
/// × 18 data carriers × 2 sidebands), in the order the C reference
/// (sync.c sync_process_fm) writes them into buffer_pm: per row, LB
/// partitions in ascending bin order (j=1..18 within each), then UB
/// partitions also in ascending bin order starting from
/// `UB_END - PM_PARTITIONS*PARTITION_WIDTH_FM`. The PM interleaver
/// indexes data with `partition * C + column`, so partition slot 10
/// (PM_V's first UB entry) must correspond to the *lowest* UB
/// partition, not the highest.
pub static P1_DATA_BINS_FM: LazyLock<Vec<usize>> = LazyLock::new(|| {
    let mut v = Vec::with_capacity(2 * PM_PARTITIONS * PARTITION_DATA_CARRIERS);
    for i in 0..PM_PARTITIONS {
        let base = LB_START + i * PARTITION_WIDTH_FM;
        for j in 1..PARTITION_WIDTH_FM {
            v.push(base + j);
        }
    }
    for i in 0..PM_PARTITIONS {
        let base = UB_END - (PM_PARTITIONS - i) * PARTITION_WIDTH_FM;
        for j in 1..PARTITION_WIDTH_FM {
            v.push(base + j);
        }
    }
    v
});

/// Reference subcarrier ID (0..3) for partition index `i` (0..=10).
/// Embedded in bits 10/11 of the per-reference DBPSK sync template.
pub fn rsid_for_partition(i: usize) -> u8 {
    ((MIDDLE_REF_SC - i as i32) & 0x3) as u8
}

/// P1 logical channel frame length in information bits. defines.h:39.
pub const P1_FRAME_LEN_FM: usize = 146_176;
/// Encoded P1 frame length. 5/2 × P1_FRAME_LEN_FM. defines.h:43.
pub const P1_FRAME_LEN_ENCODED_FM: usize = P1_FRAME_LEN_FM * 5 / 2; // 365_440

/// Viterbi convolutional code for P1 FM. K=7, rate 1/3 generators
/// (octal 0133, 0171, 0165) → after puncturing {1,1,1,1,1,0} (period
/// 6, drop every 6th bit) the net rate is 2/5. decode.c:39-45,263.
/// Tail-biting termination.
pub const CONV_K7: usize = 7;
pub const CONV_K7_GEN: [u8; 3] = [0o133, 0o171, 0o165]; // 91, 121, 117
pub const CONV_K7_PUNCTURE_P1: [u8; 6] = [1, 1, 1, 1, 1, 0];

/// Frame descrambler initial state and polynomial. decode.c:282+.
pub const SCRAMBLER_INIT: u16 = 0x3FF;
pub const SCRAMBLER_WIDTH: u32 = 11;

/// Reed-Solomon (255,223) over GF(256). 32 parity bytes.
pub const RS_N: usize = 255;
pub const RS_K: usize = 223;
pub const RS_NROOTS: usize = RS_N - RS_K;
/// Primitive polynomial x⁸ + x⁴ + x³ + x² + 1.
pub const RS_GFPOLY: u16 = 0x11D;
/// First consecutive root and primitive element of the generator.
pub const RS_FCR: usize = 1;
pub const RS_PRIM: usize = 1;

/// AAS application IDs. NRSC-5 standard table.
pub const AAS_APP_PSD: u16 = 0x0001;
pub const AAS_APP_SIG: u16 = 0x0002;
pub const AAS_APP_LOT: u16 = 0x0004;

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-check `P1_DATA_BINS_FM` against the bin order produced by
    /// the C reference `sync.c sync_process_fm` when filling
    /// `buffer_pm`. The C code writes per row (after the LB loop):
    ///   for (i = UB_END - PM_PARTITIONS*PARTITION_WIDTH_FM;
    ///        i < UB_END; i += PARTITION_WIDTH_FM)
    ///       for (j = 1; j < PARTITION_WIDTH_FM; j++) c = buffer[i+j][n];
    /// so partition slot 10 of the PM interleaver matrix must come from
    /// bin range `UB_END-190+1 .. UB_END-190+18` (lowest UB partition),
    /// not `UB_END-19+1 .. UB_END-1` (highest).
    #[test]
    fn p1_data_bins_match_c_buffer_pm_layout() {
        let bins = &*P1_DATA_BINS_FM;
        assert_eq!(bins.len(), 2 * PM_PARTITIONS * PARTITION_DATA_CARRIERS);
        assert_eq!(bins.len(), 360);

        // First LB carrier = LB_START + 1.
        assert_eq!(bins[0], LB_START + 1);
        // Last LB carrier (index 179) = LB_START + 9*19 + 18.
        assert_eq!(bins[179], LB_START + 9 * PARTITION_WIDTH_FM + 18);

        // First UB carrier (index 180) must be the lowest UB data
        // carrier (= UB_END - PM_PARTITIONS*PARTITION_WIDTH_FM + 1).
        assert_eq!(bins[180], UB_END - PM_PARTITIONS * PARTITION_WIDTH_FM + 1);
        // Last UB carrier (index 359) must be the highest UB data
        // carrier (= UB_END - 1).
        assert_eq!(bins[359], UB_END - 1);

        // Within each UB partition slot, bins must be ascending.
        for slot in 0..PM_PARTITIONS {
            let base = UB_END - (PM_PARTITIONS - slot) * PARTITION_WIDTH_FM;
            for j in 1..PARTITION_WIDTH_FM {
                assert_eq!(bins[180 + slot * 18 + (j - 1)], base + j);
            }
        }
    }

    #[test]
    fn ref_bins_count_and_endpoints() {
        let r = &*REF_BINS_FM;
        assert_eq!(r.len(), NUM_REF_FM);
        // First LB ref is the outermost lower-sideband reference.
        assert_eq!(r[0], LB_START);
        // Innermost LB reference (index 10) sits just past the
        // 10th data partition.
        assert_eq!(
            r[PM_PARTITIONS],
            LB_START + PM_PARTITIONS * PARTITION_WIDTH_FM
        );
        // Outermost UB reference.
        assert_eq!(r[PM_PARTITIONS + 1], UB_END);
    }
}
