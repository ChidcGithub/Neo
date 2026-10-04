// Neo modification (2026-10-04): mandatory validation of TTS-free native artifacts.
// Original crate remains Apache-2.0; see LICENSE.
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};

pub const LIBS: &[&str] = &[
    "sherpa-onnx-c-api",
    "sherpa-onnx-core",
    "kaldi-decoder-core",
    "sherpa-onnx-kaldifst-core",
    "sherpa-onnx-fstfar",
    "sherpa-onnx-fst",
    "kaldi-native-fbank-core",
    "kissfft-float",
    "onnxruntime",
    "ssentencepiece_core",
];
const COMMIT: &str = "11afbd009a7f8c08f4bcf2fc1b265d0df4670fbf";
const SOURCE_SHA256: &str = "0a8db6c55dd318f4a688faba85f7760b99a6c92e8ef8864479d418531bee1ac2";
const DEPENDENCIES: &[(&str, &str, &str)] = &[
    ("kaldi_native_fbank", "https://github.com/csukuangfj/kaldi-native-fbank/archive/refs/tags/v1.22.3.tar.gz", "9176cc66fc7ce1edf85cf355b06e320c57db6297df74277f575183468893cf61"),
    ("kaldi_decoder", "https://github.com/k2-fsa/kaldi-decoder/archive/refs/tags/v0.3.0.tar.gz", "b9f34cfb4fd3b1344100eead79ef4d37aa15962274b9e3056de345021f76a1b0"),
    ("kaldifst", "https://github.com/k2-fsa/kaldifst/archive/refs/tags/v1.8.0.tar.gz", "3f247b7e5a2409071202f5e2bc6200060f66728c0a3443c03923ad2723e040b3"),
    ("openfst", "https://github.com/csukuangfj/openfst/archive/refs/tags/v1.8.5-2026-07-09.tar.gz", "2ff712a32952fcb01d351121a6bc8ccf4fdc6b2aa06ce8df2b3095dedd518c0e"),
    ("eigen", "https://gitlab.com/libeigen/eigen/-/archive/5.0.1/eigen-5.0.1.tar.gz", "e9c326dc8c05cd1e044c71f30f1b2e34a6161a3b6ecf445d56b53ff1669e3dec"),
    ("kissfft", "https://github.com/mborgerding/kissfft/archive/febd4caeed32e33ad8b2e0bb5ea77542c40f18ec.zip", "497103e664168ebe39580b757adbe616f6cf85a16572af581ca7bc42d0ab13fd"),
    ("simple-sentencepiece", "https://github.com/pkufool/simple-sentencepiece/archive/refs/tags/v0.7.tar.gz", "1748a822060a35baa9f6609f84efc8eb54dc0e74b9ece3d82367b7119fdc75af"),
    ("json", "https://github.com/nlohmann/json/archive/refs/tags/v3.12.0.tar.gz", "4b92eb0c06d10683f7447ce9406cb97cd4b453be18d7279320f7b2f025c10187"),
    ("onnxruntime", "https://github.com/csukuangfj/onnxruntime-libs/releases/download/v1.28.2/onnxruntime-win-x64-static_lib-MT-Release-1.28.2.tar.bz2", "77c6cc2a419828f450a570851d7d6d2385523c411dbe858f841a4c7338a78881"),
];

// The actual install is ~1.14 GB and its full symbol reports ~1.6 GB.
// Bound both streamed I/O and elapsed validation time; never load reports in RAM.
struct Budget {
    deadline: Instant,
    bytes_left: u64,
}

impl Budget {
    fn check(&self) -> io::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(invalid("Native evidence validation exceeded 180 seconds"));
        }
        Ok(())
    }

    fn consume(&mut self, bytes: u64) -> io::Result<()> {
        self.check()?;
        self.bytes_left = self
            .bytes_left
            .checked_sub(bytes)
            .ok_or_else(|| invalid("Native evidence exceeds 4 GiB validation budget"))?;
        Ok(())
    }
}

fn regular_metadata(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(invalid(format!(
                "Reparse point in native evidence: {}",
                path.display()
            )));
        }
    }
    if metadata.file_type().is_symlink() {
        return Err(invalid(format!(
            "Symlink in native evidence: {}",
            path.display()
        )));
    }
    Ok(metadata)
}

fn evidence_path(root: &Path, relative: &str) -> io::Result<PathBuf> {
    // Receipt paths are portable POSIX-relative names, never drive/UNC/ADS paths.
    if relative.contains(['\\', ':'])
        || relative
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.ends_with(['.', ' ']))
    {
        return Err(invalid(format!("Unsafe native evidence path: {relative}")));
    }
    regular_metadata(root)?;
    let mut path = root.to_path_buf();
    for part in relative.split('/') {
        path.push(part);
        regular_metadata(&path)?;
    }
    if !path.canonicalize()?.starts_with(root.canonicalize()?) {
        return Err(invalid("Native evidence escapes its root"));
    }
    Ok(path)
}

fn read_json(path: &Path, budget: &mut Budget) -> io::Result<Value> {
    let metadata = regular_metadata(path)?;
    if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Err(invalid(
            "Native JSON must be a regular file of at most 4 MiB",
        ));
    }
    let mut data = Vec::new();
    File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    budget.consume(data.len() as u64)?;
    if data.len() > 4 * 1024 * 1024 {
        return Err(invalid("Native JSON grew beyond 4 MiB"));
    }
    Ok(serde_json::from_slice(&data)?)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn hash(path: &Path, budget: &mut Budget) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        budget.consume(length as u64)?;
        hash.update(&buffer[..length]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn file_record(path: &Path, value: &Value, budget: &mut Budget) -> io::Result<()> {
    budget.check()?;
    let metadata = regular_metadata(path)?;
    if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 * 1024 {
        return Err(invalid(format!(
            "Not a regular artifact: {}",
            path.display()
        )));
    }
    if value["size"].as_u64() != Some(metadata.len())
        || value["sha256"].as_str() != Some(hash(path, budget)?.as_str())
    {
        return Err(invalid(format!(
            "Artifact hash/size mismatch: {}",
            path.display()
        )));
    }
    println!("cargo:rerun-if-changed={}", path.display());
    Ok(())
}

fn require(value: &Value, key: &str, expected: &str) -> io::Result<()> {
    if value[key].as_str() != Some(expected) {
        return Err(invalid(format!("ASR manifest requires {key}={expected}")));
    }
    Ok(())
}

pub fn validate(lib_dir: &Path) -> io::Result<()> {
    let mut budget = Budget {
        deadline: Instant::now() + Duration::from_secs(180),
        bytes_left: 4 * 1024 * 1024 * 1024,
    };
    let manifest_path = evidence_path(lib_dir, "neo-sherpa-asr.json")?;
    println!("cargo:rerun-if-changed={}", lib_dir.display());
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let manifest = read_json(&manifest_path, &mut budget)?;
    if manifest["schema"].as_u64() != Some(1) {
        return Err(invalid("Unsupported ASR manifest schema"));
    }
    for (key, value) in [
        ("status", "native-validated"),
        ("version", "1.13.8"),
        ("source_commit", COMMIT),
        ("target", "x86_64-pc-windows-msvc"),
        ("configuration", "Release"),
    ] {
        require(&manifest, key, value)?;
    }
    require(&manifest, "source_sha256", SOURCE_SHA256)?;
    let options = &manifest["options"];
    if options.as_object().map(|v| v.len()) != Some(19) {
        return Err(invalid("Native option set mismatch"));
    }
    for (key, value) in [
        ("BUILD_SHARED_LIBS", "OFF"),
        ("CMAKE_BUILD_TYPE", "Release"),
        ("CMAKE_MSVC_RUNTIME_LIBRARY", "MultiThreaded"),
        ("SHERPA_ONNX_USE_STATIC_CRT", "ON"),
        ("SHERPA_ONNX_ENABLE_C_API", "ON"),
        ("SHERPA_ONNX_ENABLE_TTS", "OFF"),
        ("SHERPA_ONNX_ENABLE_PYTHON", "OFF"),
        ("SHERPA_ONNX_ENABLE_JNI", "OFF"),
        ("SHERPA_ONNX_ENABLE_BINARY", "OFF"),
        ("SHERPA_ONNX_BUILD_C_API_EXAMPLES", "OFF"),
        ("SHERPA_ONNX_ENABLE_TESTS", "OFF"),
        ("SHERPA_ONNX_ENABLE_PORTAUDIO", "OFF"),
        ("SHERPA_ONNX_ENABLE_WEBSOCKET", "OFF"),
        ("SHERPA_ONNX_ENABLE_SPEAKER_DIARIZATION", "OFF"),
        ("SHERPA_ONNX_ENABLE_GPU", "OFF"),
        ("SHERPA_ONNX_ENABLE_DIRECTML", "OFF"),
        (
            "SHERPA_ONNX_USE_PRE_INSTALLED_ONNXRUNTIME_IF_AVAILABLE",
            "OFF",
        ),
        ("FETCHCONTENT_FULLY_DISCONNECTED", "ON"),
        ("FETCHCONTENT_UPDATES_DISCONNECTED", "ON"),
    ] {
        require(options, key, value)?;
    }
    let expected: BTreeSet<String> = LIBS
        .iter()
        .copied()
        .chain(["sherpa-onnx-cxx-api"])
        .map(|n| format!("{n}.lib"))
        .collect();
    let libraries = manifest["libraries"]
        .as_object()
        .ok_or_else(|| invalid("Missing library hashes"))?;
    if libraries.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return Err(invalid("ASR manifest has missing/extra library names"));
    }
    let mut found = BTreeSet::new();
    for entry in fs::read_dir(lib_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type()?.is_dir() || name.to_ascii_lowercase().ends_with(".dll") {
            return Err(invalid(
                "Unexpected subdirectory/DLL in static ASR lib directory",
            ));
        }
        if name.to_ascii_lowercase().ends_with(".lib") {
            found.insert(name);
        }
    }
    if found != expected {
        return Err(invalid(
            "ASR directory contains missing/extra static libraries",
        ));
    }
    for name in &expected {
        let path = evidence_path(lib_dir, name)?;
        file_record(&path, &libraries[name], &mut budget)?;
        let mut magic = [0u8; 8];
        File::open(&path)?.read_exact(&mut magic)?;
        if &magic != b"!<arch>\n" || fs::metadata(&path)?.len() <= 8 {
            return Err(invalid(format!("Not a nonempty native archive: {name}")));
        }
    }
    let receipt_path = evidence_path(lib_dir, "neo-asr-receipt.json")?;
    file_record(&receipt_path, &manifest["receipt"], &mut budget)?;
    let receipt = read_json(&receipt_path, &mut budget)?;
    for (key, value) in [
        ("version", "1.13.8"),
        ("commit", COMMIT),
        ("sha256", SOURCE_SHA256),
        ("url", "https://codeload.github.com/k2-fsa/sherpa-onnx/tar.gz/11afbd009a7f8c08f4bcf2fc1b265d0df4670fbf"),
        ("archive", "sherpa-onnx-11afbd009a7f8c08f4bcf2fc1b265d0df4670fbf.tar.gz"),
    ] {
        require(&receipt["source_lock"], key, value)?;
    }
    let dependencies = receipt["dependency_archives"]
        .as_object()
        .ok_or_else(|| invalid("Missing pinned dependency archives"))?;
    if dependencies.len() != DEPENDENCIES.len() {
        return Err(invalid("Dependency archive set mismatch"));
    }
    for (name, url, sha) in DEPENDENCIES {
        let entry = &receipt["dependency_archives"][name];
        require(entry, "url", url)?;
        require(entry, "sha256", sha)?;
        if entry.as_object().map(|v| v.len()) != Some(2) {
            return Err(invalid("Unexpected dependency archive fields"));
        }
    }
    if receipt["options"] != *options {
        return Err(invalid("Receipt/options mismatch"));
    }
    for stage in [
        "configure.command.json",
        "build.command.json",
        "install.command.json",
    ] {
        let command = &receipt["commands"][stage];
        if command["returncode"].as_i64() != Some(0) || command["timed_out"].as_bool() == Some(true)
        {
            return Err(invalid(format!(
                "No successful native stage receipt: {stage}"
            )));
        }
    }
    let reports = receipt["symbol_reports"]
        .as_object()
        .ok_or_else(|| invalid("Missing native symbol reports"))?;
    let report_names: BTreeSet<_> = LIBS
        .iter()
        .copied()
        .chain(["sherpa-onnx-cxx-api"])
        .map(|n| format!("{n}.symbols.txt"))
        .collect();
    if reports.keys().cloned().collect::<BTreeSet<_>>() != report_names {
        return Err(invalid("Native symbol report set mismatch"));
    }
    let licenses = receipt["licenses"]
        .as_object()
        .ok_or_else(|| invalid("Missing native license evidence"))?;
    if licenses.is_empty() || licenses.len() > 4096 {
        return Err(invalid("Missing/excessive native license evidence"));
    }
    let prefix = lib_dir
        .parent()
        .ok_or_else(|| invalid("Missing install root"))?;
    let attempt = prefix
        .parent()
        .ok_or_else(|| invalid("Missing native attempt root"))?;
    for (relative, key) in [
        ("graph.json", "graph"),
        ("omitted-source-links.json", "omitted_source_links"),
    ] {
        file_record(
            &evidence_path(attempt, relative)?,
            &receipt[key],
            &mut budget,
        )?;
    }
    for (name, info) in reports {
        file_record(
            &evidence_path(attempt, &format!("symbols/{name}"))?,
            info,
            &mut budget,
        )?;
    }
    for (name, info) in licenses {
        if !name.starts_with("licenses/") {
            return Err(invalid("Invalid native license evidence path"));
        }
        file_record(&evidence_path(prefix, name)?, info, &mut budget)?;
    }
    budget.check()
}
