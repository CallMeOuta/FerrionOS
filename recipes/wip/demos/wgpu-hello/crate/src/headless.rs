
mod triangle;

use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;

const SIZE: u32 = 256;

fn main() -> ExitCode {
    let path = std::env::args().nth(1).unwrap_or_else(|| "wgpu-hello.png".into());
    let gpu = match triangle::Gpu::new() {
        Ok(gpu) => gpu,
        Err(err) => {
            eprintln!("wgpu-hello-headless: {err}");
            return ExitCode::FAILURE;
        }
    };
    let info = &gpu.adapter_info;
    println!(
        "adapter: {} ({:?}, {:?}, driver {} {})",
        info.name, info.device_type, info.backend, info.driver, info.driver_info
    );

    let pixels = gpu.render(SIZE, SIZE, 0.0);
    let center = (SIZE / 2 * SIZE + SIZE / 2) as usize * 4;
    println!("center pixel: {:?}", &pixels[center..center + 4]);

    let file = match File::create(&path) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("wgpu-hello-headless: {path}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut encoder = png::Encoder::new(BufWriter::new(file), SIZE, SIZE);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    if let Err(err) = encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&pixels))
    {
        eprintln!("wgpu-hello-headless: {path}: {err}");
        return ExitCode::FAILURE;
    }
    println!("wrote {path}");
    ExitCode::SUCCESS
}
