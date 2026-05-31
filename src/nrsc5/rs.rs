//! Reed-Solomon (255, 247) decoder for NRSC-5.
//!
//! Pure-Rust port of Phil Karn's libfec character codec. Parameters match
//! the canonical nrsc5 invocation `init_rs_char(8, 0x11d, 1, 1, 8)`:
//! GF(2^8) with primitive polynomial `x^8 + x^4 + x^3 + x^2 + 1`, first
//! consecutive root α^1, primitive element α^1, eight parity bytes.
//!
//! Callers may pass a *shortened* codeword by setting `pad` to the number
//! of virtual leading zero bytes; the decoder treats them as zero without
//! any allocation. Total codeword length (`pad + data.len()`) must be 255.
//!
//! Algorithm: Horner-form syndromes → Berlekamp-Massey error locator →
//! Chien search for roots → Forney algorithm for error values. No erasure
//! support (not needed for the NRSC-5 AAS/PDU call sites).

const NN: usize = 255;
const NROOTS: usize = 8;
const FCR: u32 = 1;
const PRIM: u32 = 1;
const IPRIM: u32 = 1; // prim-th root of 1, divided by prim; for prim=1 → 1
const GFPOLY: u32 = 0x11d;
const A0: u8 = 255; // sentinel: log(0) = -∞

#[inline]
fn modnn(x: u32) -> u32 {
    x % (NN as u32)
}

pub struct RsDecoder {
    alpha_to: [u8; 256],
    index_of: [u8; 256],
    #[allow(dead_code)]
    genpoly: [u8; NROOTS + 1], // index form; used by the test encoder
}

impl Default for RsDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl RsDecoder {
    pub fn new() -> Self {
        let mut alpha_to = [0u8; 256];
        let mut index_of = [0u8; 256];

        index_of[0] = A0;
        let mut sr: u32 = 1;
        for i in 0..(NN as u32) {
            index_of[sr as usize] = i as u8;
            alpha_to[i as usize] = sr as u8;
            sr <<= 1;
            if sr & 0x100 != 0 {
                sr ^= GFPOLY;
            }
            sr &= NN as u32;
        }
        alpha_to[NN] = 0;

        // Build generator polynomial g(x) = ∏ (x − α^((FCR+i)·PRIM)) in poly form.
        let mut gp = [0u8; NROOTS + 1];
        gp[0] = 1;
        let mut root = FCR * PRIM;
        for i in 0..NROOTS {
            gp[i + 1] = 1;
            for j in (1..=i).rev() {
                if gp[j] != 0 {
                    let log = index_of[gp[j] as usize] as u32;
                    gp[j] = gp[j - 1] ^ alpha_to[modnn(log + root) as usize];
                } else {
                    gp[j] = gp[j - 1];
                }
            }
            let log = index_of[gp[0] as usize] as u32;
            gp[0] = alpha_to[modnn(log + root) as usize];
            root += PRIM;
        }
        let mut genpoly = [0u8; NROOTS + 1];
        for i in 0..=NROOTS {
            genpoly[i] = index_of[gp[i] as usize];
        }

        Self {
            alpha_to,
            index_of,
            genpoly,
        }
    }

    /// Decode in place. `data` is the (possibly shortened) codeword, and
    /// `pad` is the number of virtual leading zero bytes that bring the
    /// total length up to 255. Returns the number of errors corrected, or
    /// `Err(())` if the codeword is uncorrectable.
    pub fn decode(&self, data: &mut [u8], pad: usize) -> Result<usize, ()> {
        if pad + data.len() != NN {
            return Err(());
        }

        // ---- syndromes -------------------------------------------------
        // Horner-form evaluation at α^((FCR+i)·PRIM). Virtual pad bytes are
        // zero, and 0·x + 0 = 0, so we can skip them entirely: starting the
        // accumulator at zero and iterating only real bytes is equivalent.
        let mut s_poly = [0u8; NROOTS];
        for &d in data.iter() {
            for i in 0..NROOTS {
                if s_poly[i] == 0 {
                    s_poly[i] = d;
                } else {
                    let log = self.index_of[s_poly[i] as usize] as u32;
                    let exp = modnn(log + (FCR + i as u32) * PRIM);
                    s_poly[i] = d ^ self.alpha_to[exp as usize];
                }
            }
        }
        let mut syn_or = 0u8;
        let mut s = [0u8; NROOTS];
        for i in 0..NROOTS {
            syn_or |= s_poly[i];
            s[i] = self.index_of[s_poly[i] as usize];
        }
        if syn_or == 0 {
            return Ok(0);
        }

        // ---- Berlekamp-Massey ------------------------------------------
        // lambda kept in poly form during the loop; b kept in index form.
        let mut lambda = [0u8; NROOTS + 1];
        lambda[0] = 1;
        let mut b = [0u8; NROOTS + 1];
        for i in 0..=NROOTS {
            b[i] = self.index_of[lambda[i] as usize];
        }
        let mut t = [0u8; NROOTS + 1];
        let mut el: usize = 0;
        for r in 1..=NROOTS {
            let mut discr: u8 = 0;
            for i in 0..r {
                if lambda[i] != 0 && s[r - i - 1] != A0 {
                    let log_l = self.index_of[lambda[i] as usize] as u32;
                    let exp = modnn(log_l + s[r - i - 1] as u32);
                    discr ^= self.alpha_to[exp as usize];
                }
            }
            let discr_idx = self.index_of[discr as usize];
            if discr_idx == A0 {
                // B(x) ← x · B(x)
                for i in (1..=NROOTS).rev() {
                    b[i] = b[i - 1];
                }
                b[0] = A0;
            } else {
                // T(x) = λ(x) − discr · x · B(x)
                t[0] = lambda[0];
                for i in 0..NROOTS {
                    if b[i] != A0 {
                        let exp = modnn(discr_idx as u32 + b[i] as u32);
                        t[i + 1] = lambda[i + 1] ^ self.alpha_to[exp as usize];
                    } else {
                        t[i + 1] = lambda[i + 1];
                    }
                }
                if 2 * el <= r - 1 {
                    el = r - el;
                    // B(x) ← inv(discr) · λ(x)
                    for i in 0..=NROOTS {
                        if lambda[i] == 0 {
                            b[i] = A0;
                        } else {
                            let log_l = self.index_of[lambda[i] as usize] as u32;
                            b[i] = modnn(log_l + NN as u32 - discr_idx as u32) as u8;
                        }
                    }
                } else {
                    for i in (1..=NROOTS).rev() {
                        b[i] = b[i - 1];
                    }
                    b[0] = A0;
                }
                lambda.copy_from_slice(&t);
            }
        }

        // Convert λ to index form, find deg λ.
        let mut deg_lambda: usize = 0;
        let mut lambda_idx = [0u8; NROOTS + 1];
        for i in 0..=NROOTS {
            lambda_idx[i] = self.index_of[lambda[i] as usize];
            if lambda_idx[i] != A0 {
                deg_lambda = i;
            }
        }
        let lambda = lambda_idx;

        // ---- Chien search ----------------------------------------------
        let mut reg = [0u8; NROOTS + 1];
        reg[1..=NROOTS].copy_from_slice(&lambda[1..=NROOTS]);
        let mut count: usize = 0;
        let mut root_arr = [0u32; NROOTS];
        let mut loc_arr = [0u32; NROOTS];
        let mut k: u32 = (IPRIM + NN as u32 - 1) % NN as u32;
        for i in 1..=(NN as u32) {
            let mut q: u8 = 1; // λ(0) constant term = 1 always
            for j in (1..=deg_lambda).rev() {
                if reg[j] != A0 {
                    reg[j] = modnn(reg[j] as u32 + j as u32) as u8;
                    q ^= self.alpha_to[reg[j] as usize];
                }
            }
            if q == 0 {
                root_arr[count] = i;
                loc_arr[count] = k;
                count += 1;
                if count == deg_lambda {
                    break;
                }
            }
            k = modnn(k + IPRIM);
        }
        if deg_lambda != count {
            // deg(λ) ≠ number of roots → uncorrectable.
            return Err(());
        }

        // ---- ω(x) = s(x)·λ(x) mod x^NROOTS, in index form -------------
        let mut omega = [0u8; NROOTS + 1];
        let mut deg_omega: usize = 0;
        for i in 0..NROOTS {
            let mut tmp: u8 = 0;
            let jmax = if deg_lambda < i { deg_lambda } else { i };
            for j in (0..=jmax).rev() {
                if s[i - j] != A0 && lambda[j] != A0 {
                    let exp = modnn(s[i - j] as u32 + lambda[j] as u32);
                    tmp ^= self.alpha_to[exp as usize];
                }
            }
            if tmp != 0 {
                deg_omega = i;
            }
            omega[i] = self.index_of[tmp as usize];
        }
        omega[NROOTS] = A0;

        // ---- Forney error values ---------------------------------------
        // num2 = α^(root·(FCR−1) + NN). With FCR=1 this collapses to α^0 = 1,
        // i.e. log(num2) = 0, so it drops out of the final exponent sum.
        for j in (0..count).rev() {
            let mut num1: u8 = 0;
            for i in (0..=deg_omega).rev() {
                if omega[i] != A0 {
                    let exp = modnn(omega[i] as u32 + i as u32 * root_arr[j]);
                    num1 ^= self.alpha_to[exp as usize];
                }
            }
            // λ'(α^root): for char-2 fields only odd-power terms survive,
            // so we sum λ[i+1]·α^(i·root) over even i.
            let start = core::cmp::min(deg_lambda, NROOTS - 1) & !1;
            let mut den: u8 = 0;
            let mut i: i32 = start as i32;
            while i >= 0 {
                if lambda[i as usize + 1] != A0 {
                    let exp = modnn(lambda[i as usize + 1] as u32 + (i as u32) * root_arr[j]);
                    den ^= self.alpha_to[exp as usize];
                }
                i -= 2;
            }
            if den == 0 {
                return Err(());
            }
            if num1 != 0 {
                let loc = loc_arr[j] as usize;
                if loc < pad {
                    // Error reported inside the virtual zero pad: the
                    // codeword can't actually be valid, so refuse it.
                    return Err(());
                }
                let log_n1 = self.index_of[num1 as usize] as u32;
                let log_den = self.index_of[den as usize] as u32;
                let exp = modnn(log_n1 + NN as u32 - log_den);
                data[loc - pad] ^= self.alpha_to[exp as usize];
            }
        }

        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Systematic encoder (used only by tests). Mirrors libfec's
    // encode_rs_char: LFSR clocked by message ⊕ top parity byte.
    fn encode(dec: &RsDecoder, msg: &[u8], out: &mut [u8]) {
        assert_eq!(out.len(), msg.len() + NROOTS);
        out[..msg.len()].copy_from_slice(msg);
        let mut parity = [0u8; NROOTS];
        for &d in msg {
            let feedback = dec.index_of[(d ^ parity[0]) as usize];
            if feedback != A0 {
                for j in 1..NROOTS {
                    let g = dec.genpoly[NROOTS - j];
                    if g != A0 {
                        let exp = modnn(feedback as u32 + g as u32);
                        parity[j] ^= dec.alpha_to[exp as usize];
                    }
                }
            }
            for j in 0..NROOTS - 1 {
                parity[j] = parity[j + 1];
            }
            if feedback != A0 {
                let g = dec.genpoly[0];
                parity[NROOTS - 1] = if g != A0 {
                    let exp = modnn(feedback as u32 + g as u32);
                    dec.alpha_to[exp as usize]
                } else {
                    0
                };
            } else {
                parity[NROOTS - 1] = 0;
            }
        }
        out[msg.len()..].copy_from_slice(&parity);
    }

    fn make_codeword(dec: &RsDecoder, msg_len: usize) -> Vec<u8> {
        let msg: Vec<u8> = (0..msg_len).map(|i| ((i * 13) ^ 0x5A) as u8).collect();
        let mut cw = vec![0u8; msg.len() + NROOTS];
        encode(dec, &msg, &mut cw);
        cw
    }

    #[test]
    fn roundtrip_no_errors() {
        let dec = RsDecoder::new();
        let cw = make_codeword(&dec, 247);
        let original = cw.clone();
        let mut data = cw;
        let n = dec.decode(&mut data, 0).expect("decode");
        assert_eq!(n, 0);
        assert_eq!(data, original);
    }

    #[test]
    fn correct_up_to_four_errors() {
        let dec = RsDecoder::new();
        let cw = make_codeword(&dec, 247);
        let positions = [3usize, 100, 187, 250];
        let masks = [0x5Au8, 0xA3, 0x11, 0xFE];
        for num in 1..=4 {
            let mut data = cw.clone();
            for k in 0..num {
                data[positions[k]] ^= masks[k];
            }
            let n = dec.decode(&mut data, 0).expect("decode");
            assert_eq!(n, num, "{} errors corrected count", num);
            assert_eq!(data, cw, "{} errors data mismatch", num);
        }
    }

    #[test]
    fn five_errors_uncorrectable() {
        let dec = RsDecoder::new();
        let cw = make_codeword(&dec, 247);
        let mut data = cw.clone();
        let positions = [3usize, 50, 100, 187, 250];
        for &p in &positions {
            data[p] ^= 0xA5;
        }
        // Above ⌊NROOTS/2⌋ = 4 capacity. Either the decoder declares
        // uncorrectable, or it produces a different (mis-corrected)
        // codeword. It must NOT silently restore the original.
        match dec.decode(&mut data, 0) {
            Err(_) => {}
            Ok(_) => assert_ne!(data, cw, "five errors cleanly corrected"),
        }
    }

    #[test]
    fn shortened_codeword() {
        let dec = RsDecoder::new();
        let cw = make_codeword(&dec, 237);
        assert_eq!(cw.len(), 245);

        // Clean shortened codeword decodes with zero corrections.
        let mut clean = cw.clone();
        assert_eq!(dec.decode(&mut clean, 10).unwrap(), 0);
        assert_eq!(clean, cw);

        // Two errors injected at varied positions, pad = 10.
        let mut data = cw.clone();
        data[5] ^= 0x99;
        data[180] ^= 0x42;
        let n = dec.decode(&mut data, 10).expect("decode");
        assert_eq!(n, 2);
        assert_eq!(data, cw);

        // Four-error case at maximum capacity, still inside shortened buffer.
        let mut data = cw.clone();
        let positions = [0usize, 60, 150, 244];
        let masks = [0x01u8, 0xDE, 0xAD, 0xBE];
        for k in 0..4 {
            data[positions[k]] ^= masks[k];
        }
        let n = dec.decode(&mut data, 10).expect("decode");
        assert_eq!(n, 4);
        assert_eq!(data, cw);
    }
}
