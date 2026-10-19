use crate::metadata::{
    CommandBuilder, CommandSignatureGenerators, Generator, GeneratorResults,
    GeneratorResultsCollector, Suggestion, TemplateFilter,
};
use lazy_static::lazy_static;
use regex::Regex;
use serde_json::Result;
use std::collections::HashMap;

lazy_static! {
    static ref LIST_RE: Regex = Regex::new(r"^(List)|\*").unwrap();
    static ref DEVICE_RE: Regex = Regex::new("device$").unwrap();
    static ref IOS_GET_DEVICES_RE: Regex = Regex::new(r"\([\w\d\-]+\)$").unwrap();
    static ref GRADLE_TASKS_RE: Regex = Regex::new(r"^\w+ \- |\*/").unwrap();
}

#[derive(serde::Deserialize)]
struct XcodeBuildProject {
    #[serde(default)]
    schemes: Vec<String>,

    #[serde(default)]
    configurations: Vec<String>,
}

#[derive(serde::Deserialize)]
struct XcodeBuildOutput {
    project: XcodeBuildProject,
}

#[derive(serde::Deserialize)]
struct XcRunDevice {
    name: String,
}

#[derive(serde::Deserialize)]
struct XcRunOutput {
    devices: HashMap<String, Vec<XcRunDevice>>,
}

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("react-native")
        .add_generator(
            "worker_generator",
            Generator::script(
                CommandBuilder::single_command("sysctl -n hw.ncpu"),
                |output| {
                    if let Ok(val) = output.parse::<usize>() {
                        (0..val)
                            .map(|val| Suggestion::new(val.to_string()))
                            .collect_unordered_results()
                    } else {
                        GeneratorResults::default()
                    }
                },
            ),
        )
        .add_generator(
            "xcode_config_generator",
            Generator::script(
                CommandBuilder::single_command("xcodebuild -project ios/*.xcodeproj  -list -json"),
                |output| {
                    let json_output: Result<XcodeBuildOutput> = serde_json::from_str(output);
                    match json_output {
                        Ok(configurations) => configurations
                            .project
                            .configurations
                            .into_iter()
                            .map(Suggestion::new)
                            .collect_unordered_results(),
                        Err(e) => {
                            log::info!("Unable to deserialize xcode build output: {:?}", e);
                            GeneratorResults::default()
                        }
                    }
                },
            ),
        )
        .add_generator(
            "xcode_scheme_generator",
            Generator::script(
                CommandBuilder::single_command("xcodebuild -project ios/*.xcodeproj  -list -json"),
                |output| {
                    let json_output: Result<XcodeBuildOutput> = serde_json::from_str(output);
                    match json_output {
                        Ok(configurations) => configurations
                            .project
                            .schemes
                            .into_iter()
                            .map(Suggestion::new)
                            .collect_unordered_results(),
                        Err(e) => {
                            log::info!("Unable to deserialize xcode build output: {:?}", e);
                            GeneratorResults::default()
                        }
                    }
                },
            ),
        )
        .add_generator(
            "android_get_devices_generator",
            Generator::script(CommandBuilder::single_command("adb devices"), |output| {
                output
                    .split('\n')
                    .filter_map(|line| {
                        if line.is_empty() || LIST_RE.is_match(line) {
                            return None;
                        }

                        if DEVICE_RE.is_match(line) {
                            if let Some(item) = line.split("device").next() {
                                return Some(Suggestion::new(item.trim()));
                            }
                        }

                        None
                    })
                    .collect_unordered_results()
            }),
        )
        .add_generator(
            "ios_get_devices_simulator_generator",
            Generator::script(
                CommandBuilder::single_command("xcrun simctl list --json devices available"),
                |output| {
                    let json_output: Result<XcRunOutput> = serde_json::from_str(output);
                    match json_output {
                        Ok(xc_run_output) => xc_run_output
                            .devices
                            .into_values()
                            .flat_map(|devices| devices.into_iter())
                            .map(|device| Suggestion::new(device.name))
                            .collect_unordered_results(),
                        Err(e) => {
                            log::info!("Unable to deserialize xcrun output: {:?}", e);
                            GeneratorResults::default()
                        }
                    }
                },
            ),
        )
        .add_generator(
            "ios_get_devices_generator",
            Generator::script(
                CommandBuilder::single_command("xcrun xctrace list devices"),
                |output| {
                    output
                        .split('\n')
                        .filter_map(|line| {
                            if !line.is_empty() && !line.starts_with('=') {
                                if let Some(name) = IOS_GET_DEVICES_RE.split(line).next() {
                                    return Some(Suggestion::new(name.trim()));
                                }
                            }
                            None
                        })
                        .collect_unordered_results()
                },
            ),
        )
        .add_generator(
            "ios_get_devices_udid_generator",
            Generator::script(
                CommandBuilder::single_command("xcrun xctrace list devices"),
                |output| {
                    output
                        .split('\n')
                        .filter_map(|line| {
                            if !line.starts_with('=') && !line.is_empty() {
                                let words = line.split("").collect::<Vec<_>>();
                                words.last().map(|word| {
                                    let name = word.trim().replace(['(', ')'], "");
                                    Suggestion::new(name)
                                })
                            } else {
                                None
                            }
                        })
                        .collect_unordered_results()
                },
            ),
        )
        .add_generator(
            "gradle_tasks_generator",
            Generator::script(
                CommandBuilder::single_command("cd android/ && ./gradlew tasks"),
                |output| {
                    output
                        .split('\n')
                        .filter_map(|line| {
                            if GRADLE_TASKS_RE.is_match(line) {
                                let name_and_description: Vec<&str> = line.split(" - ").collect();

                                if name_and_description.len() >= 2 {
                                    return Some(Suggestion::with_description(
                                        name_and_description[0],
                                        name_and_description[1],
                                    ));
                                }
                            }
                            None
                        })
                        .collect_unordered_results()
                },
            ),
        )
        .add_filter(
            "filter-js-files",
            TemplateFilter(|suggestion, path_type| {
                (path_type.is_folder() || suggestion.exact_string.ends_with(".js"))
                    .then_some(suggestion)
            }),
        )
}
