//! Executable for parsing and printing D provider definitions.
// Copyright 2021 Oxide Computer Company

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use dtrace_parser;
use quote::quote;
use structopt::StructOpt;

/// Parse D scripts into Rust or C code, and generate FFI glue to DTrace.
#[derive(StructOpt, Debug)]
enum Cmd {
    /// Generate a build.rs script that can be used to build Rust-C-DTrace FFI glue.
    Buildgen {
        // The naive thing would be to do `cargo run -- build-script provider.d > build.rs`.
        // However, shell redirection operators create the redirection target _before_ running the
        // actual shell command. Thus a file `build.rs` shows up when Cargo runs, and it dutifully
        // tries to run it. Unfortunately, that file is empty. The `emit` option tells this tool to
        // write the file to `build.rs` for the user.
        /// Write data to either standard output or a file called `build.rs` in the current
        /// directory.
        #[structopt(short, long, default_value = "file", possible_values = &["file", "stdout"])]
        emit: String,

        /// The source D file to be parsed.
        #[structopt(parse(from_str))]
        source: PathBuf,
    },
    /// Print the formatted output of a D file specifying providers, in either C or Rust.
    Fmt {
        /// The type of formatted output to emit. Options are "rust" to emit the Rust struct and
        /// method definitions, "decl" for the declarations of C FFI functions to be called from
        /// Rust, and "defn" for the actual implementation of those C FFI functions.
        #[structopt(short, long, default_value = "rust", possible_values = &["rust", "decl", "defn"])]
        format: String,

        /// The source D file to be parsed.
        #[structopt(parse(from_str))]
        source: PathBuf,
    },
}

fn print_build_script(emit: &str, source: PathBuf) {
    let source = source
        .canonicalize()
        .expect("Could not canonicalize provider file");

    // Parse the actual D provider file
    let dfile = dtrace_parser::File::from_file(&source).unwrap();

    // Generate the related filenames for source and built artifacts.
    let source_filename = source.to_str().unwrap();
    let source_basename = source.file_stem().unwrap().to_str().unwrap();
    let header_name = format!("{}.h", source_basename);
    let source_name = format!("{}-wrapper.c", source_basename);
    let d_object_name = format!("{}.o", source_basename);
    let c_object_name = format!("{}-wrapper.o", source_basename);
    let lib_name = source_basename;

    let c_source = &[
        format!(
            "// Autogenerated C wrappers to DTrace probes in \"{}\"\n",
            source.to_str().unwrap()
        ),
        String::from("#include <stdint.h>"),
        format!("#include \"{}\"\n", header_name),
        format!("{}", dfile.to_c_definition()),
    ]
    .join("\n");

    let script = quote! {
        //! Autogenerated build.rs script to generate Rust-C-DTrace glue.
        use std::process::Command;
        use std::fs;
        use std::env;
        use std::path::PathBuf;

        fn main() {

            // Everything is done relative to OUT_DIR
            let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
            let header_path = out_dir.join(#header_name).to_str().unwrap().to_string();
            let source_path = out_dir.join(#source_name).to_str().unwrap().to_string();
            let c_object_path = out_dir.join(#c_object_name).to_str().unwrap().to_string();
            let d_object_path = out_dir.join(#d_object_name).to_str().unwrap().to_string();

            // Generate a header file for the provider, placing it in OUT_DIR
            Command::new("dtrace")
                 .arg("-h")
                 .arg("-s")
                 .arg(#source_filename)
                 .arg("-o")
                 .arg(header_path)
                 .output()
                 .expect("Failed to generate header from provider file");

            // Write out the C implementation, also in OUT_DIR
            fs::write(&source_path, #c_source).expect("Could not write C wrapper source file");

            // Compile the autogenerated C source
            cc::Build::new()
                .cargo_metadata(false)
                .file(&source_path)
                .include(&out_dir)
                .compile(#c_object_name);

            // Run `dtrace -G -s provider.d source.o`. This generates a provider.o object, which
            // contains all the DTrace machinery to register the probes with the kernel. It also
            // modifies source.o, replacing the call instructions for any defined probes with NOP
            // instructions. Note that this step is not required on macOS systems.
            #[cfg(not(target_os = "macos"))]
            Command::new("dtrace")
                .arg("-G")
                .arg("-s")
                .arg(#source_filename)
                .arg(&c_object_path)
                .arg("-o")
                .arg(&d_object_path)
                .spawn()
                .expect("Failed to run DTrace against compiled source file");

            // Generate a static library from all the above artifacts.
            if cfg!(target_os = "macos") {
                cc::Build::new()
                    .object(&c_object_path)
                    .compile(#lib_name);
            } else {
                cc::Build::new()
                    .object(&c_object_path)
                    .object(&d_object_path)
                    .compile(#lib_name);
            }

            // Notify cargo when to rerun the D provider file changes. The library is automatically
            // linked in by the cc::Build step.
            println!("cargo:rerun-if-changed={}", #source_filename);
        }
    }
    .to_string();

    let fmt = Command::new("rustfmt")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let script = if let Ok(mut fmt) = fmt {
        fmt.stdin
            .take()
            .unwrap()
            .write(script.as_bytes())
            .expect("Could not write rustfmt input");
        String::from_utf8(fmt.wait_with_output().unwrap().stdout).unwrap()
    } else {
        script
    };
    if emit == "file" {
        fs::write("build.rs", script).expect("Could not write build.rs script");
    } else {
        println!("{}", script);
    }
}

fn print_formatted_output(format: &str, source: PathBuf) {
    let file = dtrace_parser::File::from_file(&source).expect("Could not parse DTrace file");
    println!(
        "{}",
        match format {
            "rust" => file.to_rust_impl(),
            "decl" => file.to_c_declaration(),
            "defn" => file.to_c_definition(),
            _ => unreachable!(),
        }
    );
}

fn main() {
    let cmd = Cmd::from_args();
    match cmd {
        Cmd::Buildgen { emit, source } => print_build_script(&emit, source),
        Cmd::Fmt { format, source } => print_formatted_output(&format, source),
    }
}
