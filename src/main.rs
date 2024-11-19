//
// Copyright 2024 Christopher Atherton <the8lack8ox@pm.me>
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the “Software”), to
// deal in the Software without restriction, including without limitation the
// rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
// sell copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED “AS IS”, WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
// THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
// IN THE SOFTWARE.
//

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Result, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

// Unique file names
pub fn generate_unique_file_name(dir: &Path, ext: &str) -> PathBuf {
    let mut time_val = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let mut path = dir.join(format!("{:08x}{ext}", time_val));
    while path.exists() {
        time_val = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        path = dir.join(format!("{:08x}{ext}", time_val));
    }
    path
}

// Temporary directories
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str) -> Self {
        let mut time_val = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let mut path = env::temp_dir().join(format!("{prefix}-{:08x}", time_val));
        while path.exists() {
            time_val = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos();
            path = env::temp_dir().join(format!("{prefix}-{:08x}", time_val));
        }
        fs::create_dir(&path).expect("Could not create temporary directory");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("Could not remove temporary directory");
    }
}

// TAR files
pub struct SimpleTarArchive {
    writer: Box<dyn Write>,
    mtime: u64,
}

impl SimpleTarArchive {
    const ZEROS: [u8; 1024] = [0; 1024];

    pub fn new(writer: impl Write + 'static) -> Self {
        Self {
            writer: Box::new(writer),
            mtime: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }

    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        Ok(Self::new(File::create(path)?))
    }

    pub fn write_file<P: AsRef<Path>>(&mut self, in_path: P, file_name: String) -> Result<()> {
        let file_len = in_path.as_ref().metadata()?.len() as usize;
        let mut in_file = File::open(in_path)?;

        // Create header
        let mut header = [0; 512];
        let file_name_bytes = file_name.as_bytes();
        header[..file_name_bytes.len()].copy_from_slice(file_name_bytes); // Filename
        header[100..107].copy_from_slice(b"0000444"); // Permissions
        header[108..115].copy_from_slice(b"0000000"); // Owner ID
        header[116..123].copy_from_slice(b"0000000"); // Group ID
        header[124..135].copy_from_slice(format!("{:011o}", file_len).as_bytes()); // File size
        header[136..147].copy_from_slice(format!("{:011o}", self.mtime).as_bytes()); // Modification time
        header[148..156].copy_from_slice(b"        "); // Checksum (for now)
        header[156] = b'0'; // Link indicator
        header[257..262].copy_from_slice(b"ustar"); // UStar indicator
        header[263..265].copy_from_slice(b"00"); // UStar version

        // Calculate checksum
        let checksum: u32 = header.iter().map(|x| *x as u32).sum();
        let checksum_str = format!("{:06o}\0", checksum);
        header[148..155].copy_from_slice(checksum_str.as_bytes());

        // Write header
        self.writer.write_all(&header)?;

        // Copy file
        std::io::copy(&mut in_file, &mut self.writer)?;

        // Add padding
        if file_len % 512 != 0 {
            self.writer
                .write_all(&Self::ZEROS[..512 - file_len % 512])?;
        }

        Ok(())
    }
}

impl Drop for SimpleTarArchive {
    fn drop(&mut self) {
        // End of file padding
        self.writer
            .write_all(&Self::ZEROS)
            .expect("Could not write TAR file end-of-file marker");

        // Flush
        self.writer
            .flush()
            .expect("Could not flush TAR file buffer");
    }
}

const PROGRAM_NAME: &str = "mkcbt";
const USAGE_MESSAGE: &str = "Usage: mkcbt [--avif] OUTPUT INPUT [INPUT]...";
type ArchiveType = SimpleTarArchive;

fn convert_avif(in_path: &Path, out_path: &Path) -> Child {
    match Command::new("avifenc")
        .arg(in_path)
        .arg(out_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(proc) => proc,
        Err(_) => {
            eprintln!("ERROR! Failed to run avifenc on `{}`", in_path.display());
            std::process::exit(1);
        }
    }
}

#[derive(Clone, PartialEq)]
enum Conversion {
    Copy,
    Avif,
}

struct Task {
    path: PathBuf,
    conversion: Conversion,
    convert_proc: Option<Child>,
}

impl Task {
    fn new(in_path: &Path, conversion: Conversion, work_dir: &Path) -> Self {
        match conversion {
            Conversion::Copy => Self {
                path: in_path.to_path_buf(),
                conversion,
                convert_proc: None,
            },
            Conversion::Avif => {
                let out_path = generate_unique_file_name(work_dir, ".avif");
                let proc = convert_avif(in_path, &out_path);
                Self {
                    path: out_path,
                    conversion,
                    convert_proc: Some(proc),
                }
            }
        }
    }

    fn finish(&mut self, index: usize, width: usize, archive: &mut ArchiveType) -> Result<()> {
        if let Some(ref mut child) = self.convert_proc {
            if !child.wait()?.success() {
                eprintln!("ERROR! Image encoding process returned failure");
                std::process::exit(1);
            }
        }
        let ext = match self.conversion {
            Conversion::Copy => match self.path.extension() {
                Some(ext) => {
                    String::from(".") + ext.to_string_lossy().to_ascii_lowercase().as_str()
                }
                None => String::new(),
            },
            Conversion::Avif => String::from(".avif"),
        };
        archive.write_file(&self.path, format!("{:0fill$}{ext}", index, fill = width))?;
        if self.conversion != Conversion::Copy {
            fs::remove_file(&self.path).expect("Could not remove temporary image file");
        }
        Ok(())
    }
}

fn run() -> Result<()> {
    // Check command line
    if env::args().len() < 3 {
        eprintln!("{USAGE_MESSAGE}");
        std::process::exit(1);
    }
    let conversion = match env::args().nth(1).unwrap().as_str() {
        "--avif" => Conversion::Avif,
        _ => Conversion::Copy,
    };
    let output_path;
    let cli_inputs: Vec<_>;
    if conversion == Conversion::Copy {
        output_path = env::args().nth(1).unwrap();
        cli_inputs = env::args().skip(2).map(PathBuf::from).collect();
    } else {
        if env::args().len() < 4 {
            eprintln!("{USAGE_MESSAGE}");
            std::process::exit(1);
        }
        output_path = env::args().nth(2).unwrap();
        cli_inputs = env::args().skip(3).map(PathBuf::from).collect();
    }

    // Collect inputs
    let mut inputs;
    if cli_inputs.len() == 1 && cli_inputs[0].is_dir() {
        inputs = Vec::new();
        for entry in fs::read_dir(&cli_inputs[0])? {
            let path = entry?.path();
            if !path.is_file() {
                eprintln!("ERROR! `{}` is not a file", path.display());
                std::process::exit(1);
            }
            inputs.push(path);
        }
        if inputs.is_empty() {
            eprintln!("ERROR! `{}` is empty", cli_inputs[0].display());
            std::process::exit(1);
        }
    } else {
        inputs = cli_inputs;
        for path in &inputs {
            if !path.exists() {
                eprintln!("ERROR! File `{}` does not exist", path.display());
                std::process::exit(1);
            }
            if !path.is_file() {
                eprintln!("ERROR! `{}` is not a file", path.display());
                std::process::exit(1);
            }
        }
    }
    inputs.sort();
    let width = inputs.len().to_string().len();
    let mut inputs_queue = VecDeque::from(inputs);

    // Create output file
    let mut archive = if output_path == "-" {
        ArchiveType::new(std::io::stdout())
    } else {
        ArchiveType::create(PathBuf::from(output_path))?
    };

    // Create work directory
    let work_path;
    let _work_dir;
    match conversion {
        Conversion::Copy => {
            work_path = PathBuf::new();
            _work_dir = None;
        }
        _ => {
            let tmp_dir = TempDir::new(PROGRAM_NAME);
            work_path = tmp_dir.path().to_path_buf();
            _work_dir = Some(tmp_dir);
        }
    }

    // Process
    let process_count = std::thread::available_parallelism()?.get();
    let mut task_queue = VecDeque::with_capacity(process_count);
    for _ in 0..(std::cmp::min(inputs_queue.len(), process_count) - 1) {
        let input = inputs_queue.pop_front().unwrap();
        task_queue.push_back(Task::new(&input, conversion.clone(), &work_path));
    }
    let mut index = 0;
    while let Some(input) = inputs_queue.pop_front() {
        // Submit new job
        task_queue.push_back(Task::new(&input, conversion.clone(), &work_path));

        // Finish front job
        index += 1;
        task_queue
            .pop_front()
            .unwrap()
            .finish(index, width, &mut archive)?;
    }
    // Finish rest of jobs
    while let Some(mut task) = task_queue.pop_front() {
        index += 1;
        task.finish(index, width, &mut archive)?;
    }

    Ok(())
}

fn main() {
    match run() {
        Ok(()) => (),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
