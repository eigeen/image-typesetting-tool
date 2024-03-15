use clap::Parser;
use image::{
    imageops::{self, FilterType},
    DynamicImage, GenericImageView, ImageBuffer, RgbaImage,
};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use snafu::prelude::*;
use std::thread::{self, JoinHandle};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender},
};
use utils::BatchIter;

mod utils;

#[derive(Debug, Snafu)]
enum Error {
    #[snafu(display("I/O error: {}", source))]
    Io { source: std::io::Error },
    #[snafu(display("Image error: {}", source))]
    Image { source: image::ImageError },
    #[snafu(display("Input error: {}", reason))]
    Input { reason: String },
}

#[derive(Clone, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// 输入目录 默认 input
    #[arg(short, long, value_name = "DIR")]
    input: String,
    /// 输出目录 默认 output
    #[arg(short, long, value_name = "DIR")]
    output: Option<String>,
    /// 单张图片最大高度（单位：cm）
    #[arg(long, value_name = "cm")]
    height: Option<f64>,
    /// 纸张边距（单位：cm）
    #[arg(long, value_name = "cm")]
    border: Option<f64>,
    /// 图片之间的间距（单位：cm）
    #[arg(long, value_name = "cm")]
    margin: Option<f64>,
    /// PPC 每厘米像素数 默认118.11PPC=300PPI
    /// PPC与PPI同时设置时，PPI优先
    #[arg(long, value_name = "PPC")]
    ppc: Option<f64>,
    /// PPI 每英寸像素数 默认300PPI=118.11PPC
    #[arg(long, value_name = "PPI")]
    ppi: Option<f64>,
    /// 横向图片数量
    #[arg(long, value_name = "COUNT")]
    nh: Option<u32>,
    /// 纵向图片数量
    #[arg(long, value_name = "COUNT")]
    nv: Option<u32>,
}

struct Config {
    /// 每厘米像素数
    pub ppc: f64,
    /// 纸张外边距 单边 像素
    pub paper_border_px: u32,
    /// 纵向最小边距 像素
    pub min_margin_v_px: u32,
    /// 横向最小边距 像素
    pub min_margin_h_px: u32,
    /// 单图片目标高度 像素
    pub target_h_px: u32,
    /// 单图片最大高度 像素
    pub max_h_px: u32,
    /// 单图片最大宽度 像素
    pub max_w_px: u32,
    /// 横向图片数量
    pub n_h: u32,
    /// 纵向图片数量
    pub n_v: u32,
}

enum PBData {
    Stop,
    NewOutput(u64),
    NextOutput,
    NewRead(u64),
    NextRead(Option<String>),
    SetRead(u64),
    NewProcess(u64),
    NextProcess,
    SetProcess(u64),
    NewComp(u64),
    NextComp,
    SetComp(u64),
    Println(String),
}

impl Config {
    pub fn from_cli_default(cli: &Cli) -> Config {
        // 横向图片数量
        let n_h: u32 = cli.nh.unwrap_or(4);
        // 纵向图片数量
        let n_v: u32 = cli.nv.unwrap_or(3);
        // 单图片目标高度 厘米
        let target_h_cm: f64 = cli.height.unwrap_or(5.0);
        // 纸张外边距 单边 厘米
        let paper_border_cm: f64 = cli.border.unwrap_or(0.8);
        // 纵向最小边距 厘米
        let min_margin_v_cm: f64 = cli.margin.unwrap_or(0.3);
        // 横向最小边距 厘米
        let min_margin_h_cm: f64 = cli.margin.unwrap_or(0.3);
        // 每厘米像素数，默认从ppi计算，否则取ppc或默认值118.11=300ppi
        let ppc: f64 = match cli.ppi {
            Some(ppi) => ppi / 2.54,
            None => cli.ppc.unwrap_or(118.11),
        };
        // 纸张外边距 单边 像素
        let paper_border_px = (paper_border_cm * ppc).round() as u32;
        // 纵向最小边距 像素
        let min_margin_v_px = (min_margin_v_cm * ppc).round() as u32;
        // 横向最小边距 像素
        let min_margin_h_px = (min_margin_h_cm * ppc).round() as u32;
        // 单图片目标高度 像素
        let mut target_h_px = (target_h_cm * ppc).round() as u32;
        // 单图片最大高度 像素
        let max_h_px = ((21.0 - 2.0 * paper_border_cm - (n_v - 1) as f64 * min_margin_v_cm)
            / n_v as f64
            * ppc)
            .round() as u32;
        // 单图片最大宽度 像素
        let max_w_px = ((29.7 - 2.0 * paper_border_cm - (n_h - 1) as f64 * min_margin_h_cm)
            / n_h as f64
            * ppc)
            .round() as u32;

        // 验证config
        if target_h_px > max_h_px {
            println!("单图片目标高度超过最大高度，将设置为最大高度输出");
            target_h_px = max_h_px;
        };

        Config {
            ppc,
            paper_border_px,
            min_margin_v_px,
            min_margin_h_px,
            target_h_px,
            max_h_px,
            max_w_px,
            n_h,
            n_v,
        }
    }
}

fn scan_inputs(input_dir: &str) -> Result<Vec<PathBuf>, Error> {
    let path = Path::new(input_dir);
    let mut inputs: Vec<PathBuf> = Vec::new();
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => {
            return Err(Error::Input {
                reason: format!("输入目录`{}`不存在或无法读取", path.display()),
            })
        }
    };

    for entry in entries {
        let entry = entry.context(IoSnafu)?;
        let file_path = entry.path();
        if file_path.is_file() {
            inputs.push(file_path);
        }
    }
    Ok(inputs)
}

fn load_images(inputs: &[PathBuf], tx: Sender<PBData>) -> Result<Vec<DynamicImage>, Error> {
    let images: Result<Vec<_>, _> = inputs
        .iter()
        .map(|input| {
            let _ = tx.send(PBData::NextRead(
                input
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| Some(format!("读取：{name}"))),
            ));
            image::open(input).context(ImageSnafu)
        })
        .collect();
    Ok(images?)
}

fn draw_canvas(
    images: &[DynamicImage],
    cfg: &Config,
    tx: Sender<PBData>,
) -> Result<RgbaImage, Error> {
    // 图像预处理
    let images: Vec<DynamicImage> = images
        .iter()
        .map(|image| {
            let _ = tx.send(PBData::NextProcess);
            // 判断图片方向 旋转
            let (width, height) = image.dimensions();
            let image = if height > width {
                image.rotate270()
            } else {
                image.clone()
            };
            // resize 统一高度
            image.resize(cfg.max_w_px, cfg.target_h_px, FilterType::Lanczos3)
        })
        .collect();

    // 布局
    let mut canvas: RgbaImage = ImageBuffer::new(
        (cfg.ppc * 29.7).ceil() as u32,
        (cfg.ppc * 21.0).ceil() as u32,
    );
    images.iter().enumerate().for_each(|(i, image)| {
        let _ = tx.send(PBData::NextComp);
        let (row, col) = row_and_col_from_index(cfg.n_h as usize, i);
        let x = cfg.paper_border_px + col * (cfg.max_w_px + cfg.min_margin_h_px);
        let y = cfg.paper_border_px + row * (cfg.max_h_px + cfg.min_margin_v_px);
        imageops::overlay(&mut canvas, image, x as i64, y as i64);
    });

    Ok(canvas)
}

fn process_with_pb() -> Result<(), Error> {
    let cli = Cli::parse();

    let inputs = scan_inputs(&cli.input)?;
    let config = Config::from_cli_default(&cli);
    // 准备输出
    let output_dir = cli.output.unwrap_or("output".to_string());
    let _ = fs::remove_dir_all(&output_dir);
    fs::create_dir_all(&output_dir).context(IoSnafu)?;
    // 初始化进度条功能
    let n_input = inputs.len() as u64;
    let n_batch = (n_input as f64 / 12 as f64).ceil() as u64;
    let (handle, tx) = init_pb_thread();
    let _ = tx.send(PBData::NewOutput(n_batch));

    // 分批绘制
    let batch_size = (config.n_h * config.n_v) as usize;
    let batch_inputs_iter = BatchIter::new(inputs.into_iter(), batch_size);
    for (i, batch_inputs) in batch_inputs_iter.enumerate() {
        let n = batch_inputs.len() as u64;
        let _ = tx.send(PBData::NewRead(n));
        let _ = tx.send(PBData::NewProcess(n));
        let _ = tx.send(PBData::NewComp(n));
        let _ = tx.send(PBData::SetRead(0));
        let _ = tx.send(PBData::SetProcess(0));
        let _ = tx.send(PBData::SetComp(0));

        let images = load_images(&batch_inputs, tx.clone())?;
        let canvas = draw_canvas(&images, &config, tx.clone())?;
        let output_path = format!("{}/output_{}.png", output_dir, i);
        canvas.save(output_path).context(ImageSnafu)?;
        let _ = tx.send(PBData::NextOutput);
    }

    let _ = tx.send(PBData::Println("Done!".to_string()));
    let _ = tx.send(PBData::Stop);
    let _ = handle.join();
    Ok(())
}

fn init_pb_thread() -> (JoinHandle<()>, Sender<PBData>) {
    let (tx, rx) = mpsc::channel::<PBData>();
    let handle = thread::spawn(move || {
        let m = MultiProgress::new();
        let sty = ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}",
        )
        .unwrap()
        .progress_chars("##-");

        let pb_output = m.add(ProgressBar::new(0));
        pb_output.set_style(sty.clone());
        pb_output.set_message("输出");
        let pb_read = m.add(ProgressBar::new(0));
        pb_read.set_style(sty.clone());
        pb_read.set_message("读取图片");
        let pb_process = m.add(ProgressBar::new(0));
        pb_process.set_style(sty.clone());
        pb_process.set_message("处理图片");
        let pb_comp = m.add(ProgressBar::new(0));
        pb_comp.set_style(sty);
        pb_comp.set_message("排版图片");

        // event loop
        loop {
            match rx.recv() {
                Ok(PBData::Stop) => {
                    m.remove(&pb_read);
                    m.remove(&pb_process);
                    m.remove(&pb_comp);
                    break;
                }
                Ok(PBData::NewOutput(n)) => {
                    pb_output.set_length(n);
                    pb_output.reset();
                }
                Ok(PBData::NewRead(n)) => {
                    pb_read.set_length(n);
                    pb_read.reset();
                }
                Ok(PBData::NewProcess(n)) => {
                    pb_process.set_length(n);
                    pb_process.reset();
                }
                Ok(PBData::NewComp(n)) => pb_comp.set_length(n),
                Ok(PBData::NextOutput) => pb_output.inc(1),
                Ok(PBData::NextRead(msg)) => {
                    pb_read.inc(1);
                    if pb_read.position() == pb_read.length().unwrap_or(0) {
                        pb_read.finish_with_message("读取完成");
                        continue;
                    };
                    if let Some(msg) = msg {
                        pb_read.set_message(msg);
                    };
                }
                Ok(PBData::SetRead(n)) => pb_read.set_position(n),
                Ok(PBData::Println(s)) => {
                    let _ = m.println(s);
                }
                Ok(PBData::NextProcess) => pb_process.inc(1),
                Ok(PBData::SetProcess(n)) => pb_process.set_position(n),
                Ok(PBData::NextComp) => pb_comp.inc(1),
                Ok(PBData::SetComp(n)) => pb_comp.set_position(n),
                Err(_) => break,
            };
        }
    });

    (handle, tx)
}

fn main() -> Result<(), Error> {
    if let Err(e) = process_with_pb() {
        eprintln!("{e}");
    };

    Ok(())
}

/// 通过索引获取当前行列号
///
/// 先行后列
///
/// - nh: 横向数量
/// - idx: 当前索引
fn row_and_col_from_index(nh: usize, idx: usize) -> (u32, u32) {
    let row = (idx / nh) as u32;
    let col = (idx % nh) as u32;

    (row, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_row_and_col_from_index() {
        assert!(row_and_col_from_index(4, 0) == (0, 0));
        assert!(row_and_col_from_index(4, 3) == (0, 3));
        assert!(row_and_col_from_index(4, 11) == (2, 3));
    }
}
