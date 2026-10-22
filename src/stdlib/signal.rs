//! Signal processing: the discrete Fourier transform (and its inverse) over
//! radix-2 Cooley–Tukey.
//!
//! Inputs that are not a power of two are zero-padded to the next power of
//! two; the result reports `n` so the caller can see the effective length.
//! There is no Bluestein/arbitrary-length path and no windowing — this closes
//! the "no FFT at all" gap, it does not pretend to be a DSP library.

use crate::value::{new_ref, NativeCtx, Value};

fn next_pow2(n: usize) -> usize {
    let mut p = 1usize;
    while p < n {
        p <<= 1;
    }
    p
}

/// In-place iterative radix-2 FFT. `inverse` selects the sign of the exponent
/// and applies the 1/n scaling.
fn transform(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    if n <= 1 {
        return;
    }
    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2usize;
    while len <= n {
        let ang = sign * 2.0 * std::f64::consts::PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        let mut start = 0usize;
        while start < n {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let br = re[b] * cr - im[b] * ci;
                let bi = re[b] * ci + im[b] * cr;
                let ar = re[a];
                let ai = im[a];
                re[a] = ar + br;
                im[a] = ai + bi;
                re[b] = ar - br;
                im[b] = ai - bi;
                let next_cr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next_cr;
            }
            start += len;
        }
        len <<= 1;
    }
    if inverse {
        let scale = n as f64;
        for k in 0..n {
            re[k] /= scale;
            im[k] /= scale;
        }
    }
}

fn numbers(v: &Value, who: &str) -> Result<Vec<f64>, String> {
    match v {
        Value::Vector(d) => Ok(d.lock().unwrap().data.clone()),
        Value::List(items) => {
            let items = items.lock().unwrap();
            let mut out = Vec::with_capacity(items.items.len());
            for it in items.items.iter() {
                match it {
                    Value::Int(i) => out.push(*i as f64),
                    Value::Float(f) => out.push(*f),
                    other => {
                        return Err(format!(
                            "{} expects numbers, found {}",
                            who,
                            crate::value::type_name(other)
                        ))
                    }
                }
            }
            Ok(out)
        }
        other => Err(format!(
            "{} expects a vector or a list of numbers, found {}",
            who,
            crate::value::type_name(other)
        )),
    }
}

fn vector_value(data: Vec<f64>) -> Value {
    Value::Vector(new_ref(crate::value::collections::VectorData { data }))
}

fn result_map(re: Vec<f64>, im: Vec<f64>) -> Value {
    let n = re.len();
    let entries: Vec<(crate::value::MapKey, Value)> = vec![
        (crate::value::MapKey::Str("re".into()), vector_value(re)),
        (crate::value::MapKey::Str("im".into()), vector_value(im)),
        (crate::value::MapKey::Str("n".into()), Value::Int(n as i64)),
    ];
    Value::Map(new_ref(
        crate::value::collections::seq::MapData::from_entries(entries),
    ))
}

/// `fft(signal)` -> `{re, im, n}`. Zero-pads to a power of two.
fn n_fft(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("fft expects 1 argument, got {}", args.len()));
    }
    let mut re = numbers(&args[0], "fft")?;
    let n = next_pow2(re.len().max(1));
    re.resize(n, 0.0);
    let mut im = vec![0.0f64; n];
    transform(&mut re, &mut im, false);
    Ok(result_map(re, im))
}

/// `ifft(re, im)` -> `{re, im, n}`. `im` may be omitted (all zeros).
fn n_ifft(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(format!(
            "ifft expects 1 or 2 arguments (re, im), got {}",
            args.len()
        ));
    }
    let mut re = numbers(&args[0], "ifft")?;
    let mut im = if args.len() == 2 {
        numbers(&args[1], "ifft")?
    } else {
        vec![0.0; re.len()]
    };
    if re.len() != im.len() {
        return Err(format!(
            "ifft: real and imaginary parts differ in length ({} vs {})",
            re.len(),
            im.len()
        ));
    }
    let n = next_pow2(re.len().max(1));
    re.resize(n, 0.0);
    im.resize(n, 0.0);
    transform(&mut re, &mut im, true);
    Ok(result_map(re, im))
}

pub fn native_defs() -> Vec<crate::stdlib::NativeDef> {
    vec![("fft", n_fft), ("ifft", n_ifft)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &[f64], b: &[f64], tol: f64) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
    }

    #[test]
    fn impulse_has_a_flat_spectrum() {
        let mut re = vec![1.0, 0.0, 0.0, 0.0];
        let mut im = vec![0.0; 4];
        transform(&mut re, &mut im, false);
        assert!(close(&re, &[1.0, 1.0, 1.0, 1.0], 1e-12));
        assert!(close(&im, &[0.0; 4], 1e-12));
    }

    #[test]
    fn cosine_lands_on_its_bin() {
        // cos(2*pi*k/N) at N=8 is a single frequency bin (1 and N-1).
        let n = 8;
        let mut re: Vec<f64> = (0..n)
            .map(|k| (2.0 * std::f64::consts::PI * k as f64 / n as f64).cos())
            .collect();
        let mut im = vec![0.0; n];
        transform(&mut re, &mut im, false);
        assert!((re[1] - 4.0).abs() < 1e-9, "bin 1 = {}", re[1]);
        assert!((re[7] - 4.0).abs() < 1e-9, "bin 7 = {}", re[7]);
        for (k, &v) in re.iter().enumerate().take(7).skip(2) {
            assert!(v.abs() < 1e-9, "bin {} = {}", k, v);
        }
    }

    #[test]
    fn inverse_round_trips() {
        let orig = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let mut re = orig.clone();
        let mut im = vec![0.0; orig.len()];
        transform(&mut re, &mut im, false);
        transform(&mut re, &mut im, true);
        assert!(close(&re, &orig, 1e-12));
        assert!(close(&im, &vec![0.0; orig.len()], 1e-12));
    }

    #[test]
    fn non_power_of_two_is_padded() {
        assert_eq!(next_pow2(0), 1);
        assert_eq!(next_pow2(5), 8);
        assert_eq!(next_pow2(8), 8);
    }
}
