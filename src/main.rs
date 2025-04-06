//
// Copyright 2024-2025 Christopher Atherton <the8lack8ox@pm.me>
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
use std::io::{Error, ErrorKind, Result, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

// Temporary directories
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
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

    fn path(&self) -> &Path {
        self.path.as_path()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("Could not remove temporary directory");
    }
}

// Basic TAR files
struct SimpleTarArchive {
    writer: Box<dyn Write>,
}

impl SimpleTarArchive {
    const ZEROS: [u8; 1024] = [0; 1024];

    fn new(writer: impl Write + 'static) -> Self {
        Self {
            writer: Box::new(writer),
        }
    }

    fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        Ok(Self::new(File::create(path)?))
    }

    fn write_file<P: AsRef<Path>>(&mut self, path: P, file_name: &str) -> Result<()> {
        let file_len = path.as_ref().metadata()?.len();
        let mut file = File::open(path)?;

        // Create header
        let mut header = [0; 512];
        header[..file_name.len()].copy_from_slice(file_name.as_bytes()); // Filename
        header[100..107].copy_from_slice(b"0000444"); // Permissions
        header[108..115].copy_from_slice(b"0000000"); // Owner ID
        header[116..123].copy_from_slice(b"0000000"); // Group ID
        header[124..135].copy_from_slice(format!("{:011o}", file_len).as_bytes()); // File size
        header[136..147].copy_from_slice(b"00000000000"); // Modification time
        header[148..156].copy_from_slice(b"        "); // Checksum (for now)
        header[156] = b'0'; // Link indicator
        header[257..262].copy_from_slice(b"ustar"); // UStar indicator
        header[263..265].copy_from_slice(b"00"); // UStar version

        // Calculate checksum
        let checksum: u32 = header.iter().map(|x| *x as u32).sum();
        header[148..155].copy_from_slice(format!("{:06o}\0", checksum).as_bytes());

        // Write header
        self.writer.write_all(&header)?;

        // Copy file
        std::io::copy(&mut file, &mut self.writer)?;

        // Add padding
        if file_len % 512 != 0 {
            self.writer
                .write_all(&Self::ZEROS[..(512 - file_len % 512) as usize])?;
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

enum CbtWriterJob {
    Copy(PathBuf, usize),
    Convert(Child, PathBuf, usize),
}

struct CbtWriter {
    tar: SimpleTarArchive,
    jobs: VecDeque<CbtWriterJob>,
    index: usize,
    padding: usize,
    processes: usize,
    work_dir: TempDir,
}

impl CbtWriter {
    fn new(writer: impl Write + 'static, padding: usize) -> Result<Self> {
        let processes = std::thread::available_parallelism()?.get();
        Ok(Self {
            tar: SimpleTarArchive::new(writer),
            jobs: VecDeque::with_capacity(processes),
            index: 1,
            padding,
            processes,
            work_dir: TempDir::new("mkcbt"),
        })
    }

    fn create<P: AsRef<Path>>(path: P, padding: usize) -> Result<Self> {
        let processes = std::thread::available_parallelism()?.get();
        Ok(Self {
            tar: SimpleTarArchive::create(path)?,
            jobs: VecDeque::with_capacity(processes),
            index: 1,
            padding,
            processes,
            work_dir: TempDir::new("mkcbt"),
        })
    }

    fn submit(&mut self, path: &Path) -> Result<()> {
        while self.jobs.len() >= self.processes {
            let job = self.jobs.pop_front().unwrap();
            match job {
                CbtWriterJob::Copy(path, index) => self
                    .tar
                    .write_file(path, &format!("{:0fill$}.avif", index, fill = self.padding))?,
                CbtWriterJob::Convert(mut proc, path, index) => {
                    if !proc.wait()?.success() {
                        return Err(Error::new(ErrorKind::Other, "avifenc returned failure"));
                    }
                    self.tar.write_file(
                        &path,
                        &format!("{:0fill$}.avif", index, fill = self.padding),
                    )?;
                    fs::remove_file(path)?;
                }
            }
        }
        match path.extension() {
            Some(ext) => {
                if !ext.eq_ignore_ascii_case("avif") {
                    let tmp_path = self.work_dir.path().join(format!(
                        "{:0fill$}.avif",
                        self.index,
                        fill = self.padding
                    ));
                    self.jobs.push_back(CbtWriterJob::Convert(
                        Command::new("avifenc")
                            .args(["--jobs", "1"])
                            .args(["--speed", "0"])
                            .arg(path)
                            .arg(&tmp_path)
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn()?,
                        tmp_path,
                        self.index,
                    ))
                } else {
                    self.jobs
                        .push_back(CbtWriterJob::Copy(path.to_path_buf(), self.index));
                }
            }
            None => {
                let tmp_path = self.work_dir.path().join(format!(
                    "{:0fill$}.avif",
                    self.index,
                    fill = self.padding
                ));
                self.jobs.push_back(CbtWriterJob::Convert(
                    Command::new("avifenc")
                        .args(["--jobs", "1"])
                        .args(["--speed", "0"])
                        .arg(path)
                        .arg(&tmp_path)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()?,
                    tmp_path,
                    self.index,
                ))
            }
        }
        self.index += 1;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        while let Some(job) = self.jobs.pop_front() {
            match job {
                CbtWriterJob::Copy(path, index) => self
                    .tar
                    .write_file(path, &format!("{:0fill$}.avif", index, fill = self.padding))?,
                CbtWriterJob::Convert(mut proc, path, index) => {
                    if !proc.wait()?.success() {
                        return Err(Error::new(ErrorKind::Other, "avifenc returned failure"));
                    }
                    self.tar.write_file(
                        &path,
                        &format!("{:0fill$}.avif", index, fill = self.padding),
                    )?;
                    fs::remove_file(path)?;
                }
            }
        }
        Ok(())
    }
}

fn run() -> Result<()> {
    if env::args().len() < 3 {
        eprintln!("USAGE: mkcbt OUTPUT.cbt INPUTS...");
        std::process::exit(1);
    }

    let cl_inputs: Vec<_> = env::args().skip(2).map(PathBuf::from).collect();
    let mut inputs = Vec::new();
    for cl_input in cl_inputs {
        if !cl_input.exists() {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("'{}' does not exist", cl_input.display()),
            ));
        }
        if cl_input.is_dir() {
            let mut files: Vec<_> = fs::read_dir(cl_input)?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|path| path.is_file())
                .collect();
            files.sort();
            inputs.append(&mut files);
        } else {
            inputs.push(cl_input);
        }
    }

    let output = env::args().nth(1).unwrap();
    let mut cbt = if output == "-" {
        CbtWriter::new(std::io::stdout(), inputs.len().to_string().len())?
    } else {
        CbtWriter::create(output, inputs.len().to_string().len())?
    };

    for file in inputs {
        cbt.submit(file.as_path())?;
    }
    cbt.finish()?;

    Ok(())
}

fn main() {
    match run() {
        Ok(()) => (),
        Err(err) => {
            eprintln!("ERROR: {err}");
            std::process::exit(1);
        }
    }
}
