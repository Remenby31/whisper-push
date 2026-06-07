//! Pairwise DTW distance matrix over WAV files — the autonomous validation loop
//! for the acoustic dictionary on REAL speech.
//!
//!   cargo run -p whisper-push-acoustic --example acoustic_eval -- a.wav b.wav ...

use whisper_push_acoustic::{distance, fingerprint};

fn read_wav(path: &str) -> (u32, Vec<f32>) {
    let mut r = hound::WavReader::open(path).expect("open wav");
    let spec = r.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.unwrap() as f32 / max).collect()
        }
    };
    // Downmix to mono if needed.
    let mono: Vec<f32> = if spec.channels > 1 {
        samples
            .chunks(spec.channels as usize)
            .map(|c| c.iter().sum::<f32>() / c.len() as f32)
            .collect()
    } else {
        samples
    };
    (spec.sample_rate, mono)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let items: Vec<(String, _)> = args
        .iter()
        .map(|p| {
            let (sr, s) = read_wav(p);
            let name = std::path::Path::new(p)
                .file_stem()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone());
            (name, fingerprint(&s, sr))
        })
        .collect();

    print!("{:>10}", "");
    for (n, _) in &items {
        print!("{:>10}", n);
    }
    println!();
    for (na, fa) in &items {
        print!("{na:>10}");
        for (_, fb) in &items {
            print!("{:>10.2}", distance(fa, fb));
        }
        println!();
    }
}
