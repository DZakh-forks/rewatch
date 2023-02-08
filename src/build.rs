use crate::bsconfig;
use crate::helpers::*;
use crate::package_tree;
use ahash::{AHashMap, AHashSet};
use convert_case::{Case, Casing};
use rayon::prelude::*;
use std::fs;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub enum SourceType {
    Interface,
    Implementation,
    MlMap,
}

#[derive(Debug, Clone)]
pub struct SourceFile {
    pub dirty: bool,
    pub source_type: SourceType,
    pub namespace: Option<String>,
    pub file_path: String,
    pub ast_path: Option<String>,
    pub ast_deps: AHashSet<String>,
    pub package: package_tree::Package,
}

// Get the rescript version no. relative to project_root + `/node_modules/rescript/rescript`
pub fn get_version(project_root: &str) -> String {
    let version_cmd = Command::new(project_root.to_owned() + "/node_modules/rescript/rescript")
        .args(["-v"])
        .output()
        .expect("failed to find version");

    std::str::from_utf8(&version_cmd.stdout)
        .expect("Could not read version from rescript")
        .replace("\n", "")
}

// fn get_ast_path(file_path: &str, root_path: &str, package_name: &str) -> String {
//     return (get_basename(&file_path.to_string()).to_owned()) + ".ast";
// }

fn contains_ascii_characters(str: &str) -> bool {
    for chr in str.chars() {
        if chr.is_ascii_alphanumeric() {
            return true;
        }
    }
    return false;
}

fn generate_ast(
    package: package_tree::Package,
    filename: &str,
    root_path: &str,
    version: &str,
) -> String {
    let file = &filename.to_string();
    let build_path_abs = get_build_path(root_path, &package.name);
    let ast_path = (get_basename(&file.to_string()).to_owned())
        + match PathBuf::from(filename)
            .extension()
            .unwrap()
            .to_str()
            .unwrap()
        {
            "resi" => ".iast",
            _ => ".ast",
        };
    let abs_node_modules_path = get_node_modules_path(root_path);

    let ppx_flags = bsconfig::flatten_ppx_flags(
        &abs_node_modules_path,
        &package.bsconfig.ppx_flags,
        &package.name,
    );

    let bsc_flags = bsconfig::flatten_flags(&package.bsconfig.bsc_flags);

    let res_to_ast_args = vec![
        vec![
            "-bs-v".to_string(),
            format!("{}", version), // TODO - figure out what these string are. - Timestamps?
        ],
        ppx_flags,
        {
            package
                .bsconfig
                .reason
                .to_owned()
                .map(|x| vec!["-bs-jsx".to_string(), format!("{}", x.react_jsx)])
                .unwrap_or(vec![])
        },
        bsc_flags,
        vec![
            "-absname".to_string(),
            "-bs-ast".to_string(),
            "-o".to_string(),
            ast_path.to_string(),
            file.to_string(),
        ],
    ]
    .concat();

    // dbg!("ARgs FLAGS:");
    // dbg!(res_to_ast_args.clone());
    /* Create .ast */
    let res_to_ast =
        Command::new(abs_node_modules_path.to_string() + "/rescript/darwinarm64/bsc.exe")
            .current_dir(build_path_abs.to_string())
            .args(res_to_ast_args)
            .output()
            .expect("Error converting .res to .ast");

    let stderr = std::str::from_utf8(&res_to_ast.stderr).expect("");
    if contains_ascii_characters(stderr) {
        println!("{}", stderr);
    }
    ast_path
}

fn read_lines(filename: String) -> io::Result<io::Lines<io::BufReader<File>>> {
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}

// Namespaces work like the following: The build system will generate a file
// called `MyModule.mlmap` which contains all modules that are in the namespace
//
// Not sure what the first line of this file is, but the next lines are names of
// the modules in the namespace you can call bsc with this file, and it will
// produce compiler assets for this file basically a module with all aliases.
// Given that this is just aliases, it doesn not need to create a mjs file.
//
// Internal modules are not accessible with the following trick, they are
// compiled to a module name such as `MyModule-MyNameSpace`.  A dash in a module
// name is not possible to make in a source file, but it's possible when
// constructing the AST, so these modules are hidden from compilation.
// in the top namespace however, we alias with the proper names

fn get_dep_modules(ast_file: &str) -> Vec<String> {
    let mut deps = Vec::new();
    if let Ok(lines) = read_lines(ast_file.to_string()) {
        // we skip the first line with is some null characters
        // the following lines in the AST are the dependency modules
        // we stop when we hit a line that starts with a "/", this is the path of the file.
        // this is the point where the dependencies end and the actual AST starts
        for line in lines.skip(1) {
            match line {
                Ok(line) => {
                    let line = line.trim().to_string();
                    if line.starts_with('/') {
                        break;
                    } else if !line.is_empty() {
                        deps.push(line);
                    }
                }
                Err(e) => println!("Error: {}", e),
            }
        }
    }
    return deps;
}

pub fn get_namespace(package: &package_tree::Package) -> Option<String> {
    if package.namespace {
        return Some(
            package
                .bsconfig
                .name
                .to_owned()
                .replace("@", "")
                .replace("/", "_")
                .to_case(Case::Pascal),
        );
    }
    return None;
}

fn gen_mlmap(
    package: &package_tree::Package,
    namespace: &str,
    modules: &Vec<String>,
    root_path: &str,
) -> String {
    let build_path_abs = get_build_path(root_path, &package.name);
    let digest = "a".repeat(16) + "\n" + &modules.join("\n");
    let file = build_path_abs + "/" + namespace + ".mlmap";
    fs::write(&file, digest).expect("Unable to write mlmap");

    file.to_string()
}

pub fn parse_and_get_dependencies(
    version: String,
    project_root: &str,
    packages: AHashMap<String, package_tree::Package>,
) -> AHashMap<String, SourceFile> {
    let mut files: AHashMap<String, SourceFile> = AHashMap::new();

    packages.iter().for_each(|(_package_name, package)| {
        get_namespace(package).iter().for_each(|namespace| {
            // generate the mlmap "AST" file for modules that have a namespace configured
            let ast_deps = package
                .source_files
                .to_owned()
                .map(|x| {
                    x.keys()
                        .map(|path| file_path_to_module_name(&path))
                        .collect::<AHashSet<String>>()
                })
                .unwrap_or(AHashSet::new());

            let mlmap = gen_mlmap(
                &package,
                namespace,
                &Vec::from_iter(ast_deps.to_owned()),
                project_root,
            );

            files.insert(
                file_path_to_module_name(&mlmap.to_owned()),
                SourceFile {
                    file_path: mlmap.to_owned(),
                    dirty: true,
                    source_type: SourceType::MlMap,
                    namespace: None,
                    ast_path: Some(mlmap.to_owned()),
                    ast_deps: ast_deps.to_owned(),
                    package: package.to_owned(),
                },
            );
        });
        match &package.source_files {
            None => (),
            Some(source_files) => source_files.iter().for_each(|(file, _)| {
                files.insert(
                    file_path_to_module_name(&file.to_owned()),
                    SourceFile {
                        file_path: file.to_owned(),
                        dirty: true,
                        source_type: {
                            match PathBuf::from(file).extension().unwrap().to_str().unwrap() {
                                "res" => SourceType::Implementation,
                                "ml" => SourceType::Implementation,
                                "re" => SourceType::Implementation,
                                "resi" => SourceType::Interface,
                                "mli" => SourceType::Interface,
                                "rei" => SourceType::Interface,
                                // perhaps crash here
                                lol => {
                                    dbg!("LOL");
                                    dbg!(lol);
                                    unreachable!();
                                }
                            }
                        },
                        namespace: if package.namespace {
                            get_namespace(package)
                        } else {
                            None
                        },
                        ast_path: None,
                        ast_deps: AHashSet::new(),
                        package: package.to_owned(),
                    },
                );
            }),
        }
    });

    files
        .par_iter()
        // .iter()
        .map(|(module_name, metadata)| match metadata.source_type {
            SourceType::MlMap => (
                module_name.to_owned(),
                metadata.ast_path.to_owned().unwrap(),
                metadata.ast_deps.to_owned(),
            ),
            SourceType::Interface | SourceType::Implementation => {
                let ast_path = generate_ast(
                    metadata.package.to_owned(),
                    &metadata.file_path.to_owned(),
                    &get_abs_path(project_root),
                    &version,
                );

                let build_path = get_build_path(project_root, &metadata.package.bsconfig.name);

                let mut ast_deps = get_dep_modules(&(build_path + "/" + &ast_path))
                    .into_iter()
                    .collect::<AHashSet<String>>();

                ast_deps.insert("Pervasives".to_owned());
                ast_deps.remove(module_name);

                (module_name.to_owned(), ast_path, ast_deps)
            }
        })
        .collect::<Vec<(String, String, AHashSet<String>)>>()
        .into_iter()
        .for_each(|(module_name, ast_path, ast_deps)| {
            files.entry(module_name).and_modify(|file| {
                file.ast_path = Some(ast_path);
                file.ast_deps = ast_deps;
            });
        });

    files
}

pub fn compile_mlmap(package: &package_tree::Package, namespace: &str, root_path: &str) {
    let abs_node_modules_path = get_node_modules_path(root_path);
    let build_path_abs = get_build_path(root_path, &package.name);

    let mlmap_name = format!("{}.mlmap", namespace);
    let args = vec![vec![
        "-w",
        "-49",
        "-color",
        "always",
        "-no-alias-deps",
        &mlmap_name,
    ]]
    .concat();

    let _ = Command::new(
        abs_node_modules_path.to_string() + &"/rescript/darwinarm64/bsc.exe".to_string(),
    )
    .current_dir(build_path_abs.to_string())
    .args(args)
    .output()
    .expect("err");
}

pub fn compile_file(
    pkg_path_abs: &str,
    abs_node_modules_path: &str,
    source: &SourceFile,
    is_interface: bool,
) {
    let build_path_abs = &(pkg_path_abs.to_string() + "/_build");

    let deps = &source
        .package
        .bsconfig
        .bs_dependencies
        .as_ref()
        .unwrap_or(&vec![])
        .into_iter()
        .map(|x| {
            vec![
                "-I".to_string(),
                abs_node_modules_path.to_string() + "/" + x + "/_build",
            ]
        })
        .collect::<Vec<Vec<String>>>();

    dbg!("BLLLLAALAL");
    dbg!(pkg_path_abs);
    dbg!(&source.file_path);
    let namespace_args = match source.namespace.to_owned() {
        Some(namespace) => vec!["-bs-ns".to_string(), namespace],
        None => vec![],
    };
    dbg!("NAMESPACE!");
    dbg!(source.namespace.to_owned());
    let read_cmi_args = if is_interface {
        vec!["-bs-read-cmi".to_string()]
    } else {
        vec![]
    };
    let implementation_args = if is_interface {
        vec![]
    } else {
        vec![
            "-bs-package-name".to_string(),
            source.package.bsconfig.name.to_owned(),
            "-bs-package-output".to_string(),
            format!(
                "es6:{}:.mjs",
                "./".to_string()
                    + Path::new(&source.file_path)
                        .strip_prefix(pkg_path_abs)
                        .unwrap()
                        .parent()
                        .unwrap()
                        .to_str()
                        .unwrap(),
            ),
        ]
    };

    let to_mjs_args = vec![
        namespace_args,
        read_cmi_args,
        vec!["-I".to_string(), ".".to_string()],
        deps.concat(),
        vec!["-warn-error".to_string(), "A".to_string()],
        implementation_args,
        // vec![
        //     "-I".to_string(),
        //     abs_node_modules_path.to_string() + "/rescript/ocaml",
        // ],
        vec![source.ast_path.to_owned().expect("No path found")],
    ]
    .concat();

    dbg!(
        abs_node_modules_path.to_string() + &"/rescript/darwinarm64/bsc.exe".to_string(),
        build_path_abs.to_string(),
        &source.ast_deps,
        &to_mjs_args
    );

    let to_mjs = Command::new(
        abs_node_modules_path.to_string() + &"/rescript/darwinarm64/bsc.exe".to_string(),
    )
    .current_dir(build_path_abs.to_string())
    .args(to_mjs_args)
    .output();

    match to_mjs {
        Ok(x) => {
            println!("STDOUT: {}", std::str::from_utf8(&x.stdout).expect(""));
            println!("STDERR: {}", std::str::from_utf8(&x.stderr).expect(""));
        }
        Err(e) => println!("ERROR, {}, {:?}", e, source.ast_path),
    }
}