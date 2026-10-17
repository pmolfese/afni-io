use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::drivesuma::{stable_id, DurationArg, RemoteCommand, SurfaceCommand};
use crate::surface_io::SurfaceFormat;
use crate::{Error, Result};

/// One action in a DriveSuma command line.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// A command transmitted as an `EngineCommand` group.
    Remote(RemoteCommand),
    /// Wait locally before processing the next action.
    Sleep(DurationArg),
    /// Ask the user locally before processing the next action.
    Pause(String),
    /// Redirect responses from subsequent query commands.
    SetOutplug(PathBuf),
    /// A complete surface or coordinate update sent as binary NIML.
    Surface(SurfaceCommand),
}

/// Parse the legacy tokens belonging to one `-com` block.
pub fn parse_command(tokens: &[String], working_dir: &Path) -> Result<Step> {
    let Some(name) = tokens.first() else {
        return Err(Error::Usage("-com requires an action".into()));
    };
    let args = &tokens[1..];
    match name.to_ascii_lowercase().as_str() {
        "sleep" => {
            let [duration] = args else {
                return Err(Error::Usage("-com sleep requires one duration".into()));
            };
            Ok(Step::Sleep(DurationArg::parse(duration)?))
        }
        "pause" => Ok(Step::Pause(args.join(" "))),
        "set_outplug" => {
            let [path] = args else {
                return Err(Error::Usage("-com set_outplug requires one path".into()));
            };
            Ok(Step::SetOutplug(PathBuf::from(path)))
        }
        "show_surf" => parse_show_surf(args, working_dir),
        "node_xyz" => parse_node_xyz(args, working_dir),
        "kill_suma" => no_arguments(name, args),
        "get_label" => no_arguments(name, args),
        "load_dset" | "load_col" => parse_data_command(name, args, working_dir),
        "surf_cont" | "viewer_cont" | "object_cont" | "recorder_cont" => {
            parse_controller(name, args, working_dir)
        }
        other => Err(Error::Usage(format!(
            "unsupported DriveSuma action {other:?}"
        ))),
    }
}

fn parse_show_surf(args: &[String], working_dir: &Path) -> Result<Step> {
    let mut label = None;
    let mut state = "iS_0".to_string();
    let mut normal_direction = 1;
    let mut input = None;
    let mut software = "Unknown".to_string();
    let mut index = 0;
    while index < args.len() {
        let raw = args[index].clone();
        let option = option_name(&raw)?;
        match option.as_str() {
            "label" | "surf_label" | "so_label" => {
                label = Some(required_value(args, &mut index, &raw)?);
            }
            "surf_state" | "state" | "so_state" => {
                state = required_value(args, &mut index, &raw)?;
            }
            "surf_winding" | "norm_dir" => {
                let value = required_value(args, &mut index, &raw)?;
                normal_direction = match value.to_ascii_lowercase().as_str() {
                    "cw" | "in" | "-1" => -1,
                    "ccw" | "out" | "1" => 1,
                    _ => {
                        return Err(Error::Usage(format!(
                            "{raw} requires cw/ccw, in/out, or -1/1"
                        )));
                    }
                };
            }
            "i" | "i_gii" | "i_fs" | "i_ply" | "i_stl" | "i_obj" | "i_byu" | "i_dx" | "i_iv"
            | "i_mni" | "i_bv" => {
                let value = required_value(args, &mut index, &raw)?;
                let path = resolve_path(working_dir, &value);
                let format = format_for_option(&option, &path)?;
                software = format_name(format).into();
                input = Some((path, None, format));
            }
            "i_1d" | "i_vec" | "i_sf" => {
                let values = take(args, &mut index, 2)?;
                let first = resolve_path(working_dir, &values[0]);
                let second = resolve_path(working_dir, &values[1]);
                let (coordinates, topology) = order_surface_pair(first, second);
                let format = if option == "i_sf" {
                    SurfaceFormat::SureFit
                } else {
                    SurfaceFormat::OneD
                };
                software = format_name(format).into();
                input = Some((coordinates, Some(topology), format));
            }
            _ => {
                return Err(Error::Usage(format!(
                    "unsupported show_surf option {raw:?}"
                )));
            }
        }
    }
    let label = label.ok_or_else(|| Error::Usage("show_surf requires -surf_label".into()))?;
    let (input, topology, format) =
        input.ok_or_else(|| Error::Usage("show_surf requires -i or -i_TYPE".into()))?;
    let side = side_from_name(&input.to_string_lossy()).to_string();
    let surface = load_surface(&input, topology.as_deref(), format)?;
    Ok(Step::Surface(SurfaceCommand::Show {
        label,
        state,
        normal_direction,
        software,
        side,
        surface,
    }))
}

fn parse_node_xyz(args: &[String], working_dir: &Path) -> Result<Step> {
    let mut label = None;
    let mut vertices = None;
    let mut index = 0;
    while index < args.len() {
        let raw = args[index].clone();
        let option = option_name(&raw)?;
        match option.as_str() {
            "label" | "surf_label" | "so_label" => {
                label = Some(required_value(args, &mut index, &raw)?);
            }
            "xyz_1d" => {
                let value = required_value(args, &mut index, &raw)?;
                let path_text = value
                    .split('[')
                    .next()
                    .unwrap_or(&value)
                    .trim_end_matches('\'');
                let table = crate::onedee::OneD::read(resolve_path(working_dir, path_text))?;
                if table.cols < 3 {
                    return Err(Error::Usage(
                        "-xyz_1D requires at least three columns".into(),
                    ));
                }
                vertices = Some(
                    (0..table.rows)
                        .map(|row| {
                            [0, 1, 2].map(|column| table.get(row, column).unwrap_or(0.0) as f32)
                        })
                        .collect(),
                );
            }
            "i" | "i_gii" | "i_fs" | "i_ply" | "i_stl" | "i_obj" | "i_byu" | "i_dx" | "i_iv"
            | "i_mni" | "i_bv" => {
                let value = required_value(args, &mut index, &raw)?;
                let path = resolve_path(working_dir, &value);
                let format = format_for_option(&option, &path)?;
                vertices = Some(load_surface(&path, None, format)?.vertices);
            }
            "i_1d" | "i_vec" | "i_sf" => {
                let values = take(args, &mut index, 2)?;
                let first = resolve_path(working_dir, &values[0]);
                let second = resolve_path(working_dir, &values[1]);
                let (coordinates, topology) = order_surface_pair(first, second);
                let format = if option == "i_sf" {
                    SurfaceFormat::SureFit
                } else {
                    SurfaceFormat::OneD
                };
                vertices = Some(load_surface(&coordinates, Some(&topology), format)?.vertices);
            }
            _ => return Err(Error::Usage(format!("unsupported node_xyz option {raw:?}"))),
        }
    }
    Ok(Step::Surface(SurfaceCommand::NodeXyz {
        label: label.ok_or_else(|| Error::Usage("node_xyz requires -surf_label".into()))?,
        vertices: vertices.ok_or_else(|| Error::Usage("node_xyz requires -xyz_1D or -i".into()))?,
    }))
}

fn resolve_path(working_dir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        working_dir.join(path)
    }
}

fn load_surface(
    path: &Path,
    topology: Option<&Path>,
    format: SurfaceFormat,
) -> Result<crate::surface::Surface> {
    if let Some(topology) = topology {
        Ok(crate::surface_io::read_surface_pair(
            path, topology, format,
        )?)
    } else {
        Ok(crate::surface_io::read_surface_as(path, format)?)
    }
}

fn format_for_option(option: &str, path: &Path) -> Result<SurfaceFormat> {
    Ok(match option {
        "i" => SurfaceFormat::from_path(path).unwrap_or(SurfaceFormat::FreeSurferBinary),
        "i_gii" => SurfaceFormat::Gifti,
        "i_fs"
            if path
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(".asc") =>
        {
            SurfaceFormat::FreeSurferAscii
        }
        "i_fs" => SurfaceFormat::FreeSurferBinary,
        "i_ply" => SurfaceFormat::Ply,
        "i_stl" => SurfaceFormat::Stl,
        "i_obj" => SurfaceFormat::WavefrontObj,
        "i_byu" => SurfaceFormat::Byu,
        "i_dx" => SurfaceFormat::OpenDx,
        "i_iv" => SurfaceFormat::OpenInventor,
        "i_mni" => SurfaceFormat::MniObj,
        "i_bv" => SurfaceFormat::BrainVoyager,
        _ => {
            return Err(Error::Usage(format!(
                "unsupported surface option -{option}"
            )));
        }
    })
}

fn format_name(format: SurfaceFormat) -> &'static str {
    match format {
        SurfaceFormat::Gifti => "GIFTI",
        SurfaceFormat::FreeSurferAscii | SurfaceFormat::FreeSurferBinary => "FreeSurfer",
        SurfaceFormat::OneD => "1D",
        SurfaceFormat::WavefrontObj => "Wavefront OBJ",
        SurfaceFormat::Ply => "PLY",
        SurfaceFormat::Stl => "STL",
        SurfaceFormat::Byu => "BYU",
        SurfaceFormat::OpenDx => "OpenDX",
        SurfaceFormat::OpenInventor => "Open Inventor",
        SurfaceFormat::MniObj => "MNI OBJ",
        SurfaceFormat::BrainVoyager => "BrainVoyager",
        SurfaceFormat::SureFit => "SureFit",
    }
}

fn order_surface_pair(first: PathBuf, second: PathBuf) -> (PathBuf, PathBuf) {
    if first
        .to_string_lossy()
        .to_ascii_lowercase()
        .ends_with(".topo")
    {
        (second, first)
    } else {
        (first, second)
    }
}

fn side_from_name(value: &str) -> &'static str {
    let value = value.to_ascii_lowercase();
    let name = Path::new(&value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&value);
    if name.starts_with("lh") || name.contains("-lh") || name.contains("left") {
        "left"
    } else if name.starts_with("rh") || name.contains("-rh") || name.contains("right") {
        "right"
    } else {
        "unknown"
    }
}

fn no_arguments(name: &str, args: &[String]) -> Result<Step> {
    if !args.is_empty() {
        return Err(Error::Usage(format!("-com {name} takes no arguments")));
    }
    Ok(Step::Remote(RemoteCommand {
        name: name.into(),
        attributes: BTreeMap::new(),
    }))
}

fn parse_controller(name: &str, args: &[String], working_dir: &Path) -> Result<Step> {
    let mut attributes = BTreeMap::new();
    let mut keys = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let raw = args[index].clone();
        let option = option_name(&raw)?;
        if option == "key" || option.starts_with("key:") {
            // Option names are case-insensitive, but values embedded in key
            // modifiers are not: `-key:v54R` must retain the capital R.
            let modifiers = raw
                .strip_prefix('-')
                .and_then(|value| value.get(3..))
                .expect("option_name validated the leading dash");
            let key = required_value(args, &mut index, &raw)?;
            keys.push(parse_key(modifiers, key)?);
            continue;
        }
        match option.as_str() {
            "label" | "surf_label" | "so_label" => {
                let value = required_value(args, &mut index, &raw)?;
                set_surface_label(&mut attributes, value)?;
            }
            "setsumaenv" => {
                let value = required_value(args, &mut index, &raw)?;
                let value = strip_matching_quotes(&value);
                let env_index = attributes
                    .keys()
                    .filter(|key| key.starts_with("ENV."))
                    .count();
                attributes.insert(format!("ENV.{env_index}"), value.into());
            }
            "switch_surf" => {
                let value = required_value(args, &mut index, &raw)?;
                set_surface_label(&mut attributes, value.clone())?;
                attributes.insert("switch_surf".into(), value);
            }
            "switch_cmap" | "load_cmap" | "dset_label" | "i_sb" | "b_sb" | "t_sb" | "t_val"
            | "dim" | "opa" | "dsp" => {
                let wire_name = match option.as_str() {
                    "i_sb" => "I_sb",
                    "b_sb" => "B_sb",
                    "t_sb" => "T_sb",
                    "t_val" => "T_val",
                    "dim" => "Dim",
                    "opa" => "Opa",
                    "dsp" => "Dsp",
                    _ => option.as_str(),
                };
                attributes.insert(wire_name.into(), required_value(args, &mut index, &raw)?);
            }
            "switch_cmode" => {
                let value = required_value(args, &mut index, &raw)?;
                if !matches!(value.to_ascii_lowercase().as_str(), "nn" | "dir" | "int") {
                    return Err(Error::Usage(format!("{raw} requires NN, Dir, or Int")));
                }
                attributes.insert("switch_cmode".into(), value);
            }
            "switch_dset" => {
                let value = required_value(args, &mut index, &raw)?;
                attributes.insert("dset_label".into(), value.clone());
                attributes.insert("switch_dset".into(), value);
            }
            "load_dset" | "load_col" => {
                let value = required_value(args, &mut index, &raw)?;
                let wire_name = if option == "load_dset" {
                    "Dset_FileName"
                } else {
                    "Col_FileName"
                };
                attributes.insert(
                    wire_name.into(),
                    resolve_path(working_dir, &value)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            "i_range" | "b_range" => {
                let wire_name = if option == "i_range" {
                    "I_range"
                } else {
                    "B_range"
                };
                attributes.insert(wire_name.into(), parse_range(args, &mut index, &raw)?);
            }
            "b_scale" | "clst" | "clust" => {
                let values = take(args, &mut index, 2)?;
                let first = parse_number(&values[0], &raw)?;
                let second = parse_number(&values[1], &raw)?;
                let (first, second) = if option == "b_scale" && first > second {
                    (second, first)
                } else {
                    (first, second)
                };
                let wire_name = if option == "b_scale" {
                    "B_scale"
                } else {
                    "Clst"
                };
                attributes.insert(wire_name.into(), format!("{first:.6} , {second:.6}"));
            }
            "bkg_col" => {
                let mut values = take(args, &mut index, 3)?;
                if index < args.len() && !looks_like_option(&args[index]) {
                    values.push(args[index].clone());
                    index += 1;
                } else {
                    values.push("1.0".into());
                }
                let values = values
                    .iter()
                    .map(|value| parse_number(value, &raw))
                    .collect::<Result<Vec<_>>>()?;
                attributes.insert(
                    "bkg_col".into(),
                    format!(
                        "{:.6} , {:.6}, {:.6}, {:.6}",
                        values[0], values[1], values[2], values[3]
                    ),
                );
            }
            "viewer" => {
                attributes.insert("SV_id".into(), required_value(args, &mut index, &raw)?);
            }
            "useclst" | "useclust" | "1_only" | "shw_0" | "show_0" | "view_dset" | "view_surf"
            | "view_surf_cont" | "view_object_cont" | "inout_notify" => {
                let value = yes_no(&required_value(args, &mut index, &raw)?, &raw)?;
                let wire_name = match option.as_str() {
                    "useclst" | "useclust" => "UseClst",
                    "show_0" | "shw_0" => "shw_0",
                    "view_surf_cont" | "view_object_cont" => "View_Surf_Cont",
                    _ => option.as_str(),
                };
                attributes.insert(wire_name.into(), value.into());
            }
            "set_func_alpha" => {
                let value = required_value(args, &mut index, &raw)?;
                let lower = value.to_ascii_lowercase();
                let (enabled, mode) = match lower.chars().next() {
                    Some('y') => ("y", "Q"),
                    Some('n') => ("n", "Q"),
                    Some('l') => ("y", "L"),
                    Some('q') => ("y", "Q"),
                    Some('o') if lower.starts_with("on") => ("y", "Q"),
                    Some('o') if lower.starts_with("of") => ("n", "Q"),
                    _ => {
                        return Err(Error::Usage(format!(
                            "{raw} requires yes/no, on/off, L, or Q"
                        )));
                    }
                };
                attributes.insert("SET_FUNC_ALPHA".into(), enabled.into());
                attributes.insert("SET_FUNC_ALPHA_MODE".into(), mode.into());
            }
            "set_func_boxed" => {
                let value = yes_no_or_on_off(&required_value(args, &mut index, &raw)?, &raw)?;
                attributes.insert("SET_FUNC_BOXED".into(), value.into());
            }
            "rendermode" => {
                let value = required_value(args, &mut index, &raw)?;
                let mode = match value.chars().next().map(|value| value.to_ascii_lowercase()) {
                    Some('v') => "Viewer",
                    Some('f') => "Fill",
                    Some('l') => "Line",
                    Some('p') => "Points",
                    Some('h') => "Hide",
                    _ => return Err(Error::Usage(format!("{raw} requires V, F, L, P, or H"))),
                };
                attributes.insert("view_surf".into(), mode.into());
            }
            "transmode" => {
                let value = required_value(args, &mut index, &raw)?;
                let transformed = if value.starts_with('v') || value.starts_with('V') {
                    "Viewer".into()
                } else if let Some(percent) = value.strip_suffix('%') {
                    let percent = bounded_integer(percent, &raw, 0, 100)?;
                    (percent * 16 / 100).to_string()
                } else {
                    bounded_integer(&value, &raw, 0, 100)?.to_string()
                };
                attributes.insert("trans_surf".into(), transformed);
            }
            "masks" | "2xmasks" | "delete_all_masks" => {
                let wire_name = match option.as_str() {
                    "masks" => "Masks",
                    "2xmasks" => "2xMasks",
                    _ => "Delete_All_Masks",
                };
                attributes.insert(wire_name.into(), "Click!".into());
                index += 1;
            }
            "load_masks" | "save_masks" => {
                let wire_name = if option == "load_masks" {
                    "Load_Masks"
                } else {
                    "Save_Masks"
                };
                attributes.insert(wire_name.into(), required_value(args, &mut index, &raw)?);
            }
            option if help_attribute(option).is_some() => {
                let wire_name = help_attribute(option).expect("guard checked the option");
                attributes.insert(wire_name.into(), required_value(args, &mut index, &raw)?);
            }
            "viewer_size" => {
                let values = take(args, &mut index, 2)?;
                bounded_integer(&values[0], &raw, 10, 2000)?;
                bounded_integer(&values[1], &raw, 10, 2000)?;
                attributes.insert("WindWidth".into(), values[0].clone());
                attributes.insert("WindHeight".into(), values[1].clone());
                attributes.insert("DoViewerSetup".into(), "y".into());
            }
            "viewer_width" | "width" => {
                let value = required_value(args, &mut index, &raw)?;
                bounded_integer(&value, &raw, 10, 2000)?;
                attributes.insert("WindWidth".into(), value);
                attributes.insert("DoViewerSetup".into(), "y".into());
            }
            "viewer_height" | "height" => {
                let value = required_value(args, &mut index, &raw)?;
                bounded_integer(&value, &raw, 10, 2000)?;
                attributes.insert("WindHeight".into(), value);
                attributes.insert("DoViewerSetup".into(), "y".into());
            }
            "viewer_position" => {
                let values = take(args, &mut index, 2)?;
                bounded_integer(&values[0], &raw, 0, 4000)?;
                bounded_integer(&values[1], &raw, 0, 4000)?;
                attributes.insert("WindX".into(), values[0].clone());
                attributes.insert("WindY".into(), values[1].clone());
                attributes.insert("DoViewerSetup".into(), "y".into());
            }
            "controller_position" => {
                let values = take(args, &mut index, 2)?;
                bounded_integer(&values[0], &raw, 0, 4000)?;
                bounded_integer(&values[1], &raw, 0, 4000)?;
                attributes.insert("ContX".into(), values[0].clone());
                attributes.insert("ContY".into(), values[1].clone());
                attributes.insert("DoViewerSetup".into(), "y".into());
            }
            "autorecord" => {
                attributes.insert("autorecord".into(), required_value(args, &mut index, &raw)?);
            }
            "load_do" | "load_view" => {
                let wire_name = if option == "load_do" {
                    "DO_FileName"
                } else {
                    "VVS_FileName"
                };
                attributes.insert(wire_name.into(), required_value(args, &mut index, &raw)?);
            }
            option if option.starts_with("fixed_do") || option.starts_with("mobile_do") => {
                let body = required_value(args, &mut index, &raw)?;
                let elements = crate::niml::parse_bytes(body.as_bytes()).map_err(|error| {
                    Error::Usage(format!("{raw} requires one valid NIML element: {error}"))
                })?;
                if elements.len() != 1 {
                    return Err(Error::Usage(format!(
                        "{raw} requires exactly one NIML element"
                    )));
                }
                let fixed = option.starts_with("fixed_do");
                let head = if fixed {
                    format!(
                        "<nido_head coord_type = 'fixed'\ndefault_color = '1.0 1.0 1.0'\ndefault_font = 'he18'\nidcode_str = {}/>\n",
                        stable_id(&raw)
                    )
                } else {
                    format!(
                        "<nido_head coord_type = 'mobile'\ndefault_SO_label = 'CURRENT'\nbond = 'surface'\nidcode_str = {}/>\n",
                        stable_id(&raw)
                    )
                };
                attributes.insert("DO_FileName".into(), format!("{head}{body}"));
            }
            "n_foreg_smooth" | "n_final_smooth" => {
                let value = required_value(args, &mut index, &raw)?;
                bounded_integer(&value, &raw, 0, 500)?;
                let wire_name = if option == "n_foreg_smooth" {
                    "N_foreg_smooth"
                } else {
                    "N_final_smooth"
                };
                attributes.insert(wire_name.into(), value);
            }
            "anim_dup" => {
                let value = required_value(args, &mut index, &raw)?;
                bounded_integer(&value, &raw, 0, i64::MAX)?;
                attributes.insert("Anim_Dup".into(), value);
            }
            "caller_working_dir" | "cwd" => {
                attributes.insert(
                    "Caller_Working_Dir".into(),
                    required_value(args, &mut index, &raw)?,
                );
            }
            "save_as" => {
                attributes.insert("Save_As".into(), required_value(args, &mut index, &raw)?);
            }
            "save_range" => {
                let values = take(args, &mut index, 2)?;
                let from = integer(&values[0], &raw)?;
                let to = integer(&values[1], &raw)?;
                attributes.insert("Save_From".into(), from.to_string());
                attributes.insert("Save_To".into(), to.to_string());
            }
            "save_last" => {
                attributes.insert("Save_From".into(), "-1".into());
                attributes.insert("Save_To".into(), "0".into());
                index += 1;
            }
            "save_index" => {
                let value = required_value(args, &mut index, &raw)?;
                let value = integer(&value, &raw)?.to_string();
                attributes.insert("Save_From".into(), value.clone());
                attributes.insert("Save_To".into(), value);
            }
            "save_last_n" => {
                let value = required_value(args, &mut index, &raw)?;
                let count = bounded_integer(&value, &raw, 1, i64::MAX)?;
                attributes.insert("Save_From".into(), (-count).to_string());
                attributes.insert("Save_To".into(), "0".into());
            }
            "save_all" => {
                attributes.insert("Save_From".into(), "0".into());
                attributes.insert("Save_To".into(), "0".into());
                index += 1;
            }
            _ => {
                return Err(Error::Usage(format!("unsupported {name} option {raw:?}")));
            }
        }
    }
    if !keys.is_empty() {
        attributes.insert("N_Key".into(), keys.len().to_string());
        for (index, key) in keys.into_iter().enumerate() {
            attributes.insert(format!("Key_{index}"), key.value);
            attributes.insert(format!("Key_rep_{index}"), key.repeat.to_string());
            attributes.insert(format!("Key_pause_{index}"), format!("{:.6}", key.pause));
            attributes.insert(
                format!("Key_redis_{index}"),
                if key.redisplay { "1" } else { "0" }.into(),
            );
            if let Some(value) = key.string_value {
                attributes.insert(format!("Key_strval_{index}"), value);
            }
        }
    }
    Ok(Step::Remote(RemoteCommand {
        name: name.into(),
        attributes,
    }))
}

fn set_surface_label(attributes: &mut BTreeMap<String, String>, value: String) -> Result<()> {
    if let Some(existing) = attributes.get("SO_label") {
        if existing != &value {
            return Err(Error::Usage(format!(
                "surface label options disagree: {existing:?} versus {value:?}"
            )));
        }
    }
    attributes.insert("SO_label".into(), value);
    Ok(())
}

fn strip_matching_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if matches!(bytes[0], b'\'' | b'"') && bytes[0] == bytes[value.len() - 1] {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn parse_range(args: &[String], index: &mut usize, option: &str) -> Result<String> {
    let mut values = take(args, index, 1)?;
    if *index < args.len() && !looks_like_option(&args[*index]) {
        values.push(args[*index].clone());
        *index += 1;
    }
    let first = parse_number(&values[0], option)?;
    let (low, high) = if values.len() == 1 {
        (-first.abs(), first.abs())
    } else {
        let second = parse_number(&values[1], option)?;
        (first.min(second), first.max(second))
    };
    Ok(format!("{low:.6} , {high:.6}"))
}

fn yes_no(value: &str, option: &str) -> Result<&'static str> {
    match value.chars().next().map(|value| value.to_ascii_lowercase()) {
        Some('y') => Ok("y"),
        Some('n') => Ok("n"),
        _ => Err(Error::Usage(format!("{option} requires y or n"))),
    }
}

fn yes_no_or_on_off(value: &str, option: &str) -> Result<&'static str> {
    let lower = value.to_ascii_lowercase();
    match lower.chars().next() {
        Some('y') => Ok("y"),
        Some('n') => Ok("n"),
        Some('o') if lower == "on" => Ok("y"),
        Some('o') if lower == "off" => Ok("n"),
        _ => Err(Error::Usage(format!("{option} requires y/n or on/off"))),
    }
}

fn integer(value: &str, option: &str) -> Result<i64> {
    value
        .parse::<i64>()
        .map_err(|_| Error::Usage(format!("{option} requires an integer")))
}

fn bounded_integer(value: &str, option: &str, low: i64, high: i64) -> Result<i64> {
    let value = integer(value, option)?;
    if (low..=high).contains(&value) {
        Ok(value)
    } else {
        Err(Error::Usage(format!(
            "{option} requires an integer in [{low}, {high}]"
        )))
    }
}

fn help_attribute(option: &str) -> Option<&'static str> {
    Some(match option {
        "write_suma_cont_help" => "Write_Suma_Cont_Help",
        "snap_suma_cont_widgets" => "Snap_Suma_Cont_Widgets",
        "write_suma_cont_sphinx_help" => "Write_Suma_Cont_Sphinx_Help",
        "write_surf_cont_help" => "Write_Surf_Cont_Help",
        "snap_surf_cont_widgets" => "Snap_Surf_Cont_Widgets",
        "write_surf_cont_sphinx_help" => "Write_Surf_Cont_Sphinx_Help",
        "write_tract_cont_help" => "Write_Tract_Cont_Help",
        "snap_tract_cont_widgets" => "Snap_Tract_Cont_Widgets",
        "write_tract_cont_sphinx_help" => "Write_Tract_Cont_Sphinx_Help",
        "write_mask_cont_help" => "Write_Mask_Cont_Help",
        "snap_mask_cont_widgets" => "Snap_Mask_Cont_Widgets",
        "write_mask_cont_sphinx_help" => "Write_Mask_Cont_Sphinx_Help",
        "write_vol_cont_help" => "Write_Vol_Cont_Help",
        "snap_vol_cont_widgets" => "Snap_Vol_Cont_Widgets",
        "write_vol_cont_sphinx_help" => "Write_Vol_Cont_Sphinx_Help",
        "write_graph_cont_help" => "Write_Graph_Cont_Help",
        "snap_graph_cont_widgets" => "Snap_Graph_Cont_Widgets",
        "write_graph_cont_sphinx_help" => "Write_Graph_Cont_Sphinx_Help",
        "write_roi_cont_help" => "Write_ROI_Cont_Help",
        "snap_roi_cont_widgets" => "Snap_ROI_Cont_Widgets",
        "write_roi_cont_sphinx_help" => "Write_ROI_Cont_Sphinx_Help",
        "write_mouse_keyb_help" => "Write_Mouse_Keyb_Help",
        "write_mouse_keyb_sphinx_help" => "Write_Mouse_Keyb_Sphinx_Help",
        "write_mouse_cmap_keyb_help" => "Write_Mouse_Cmap_Keyb_Help",
        "write_mouse_cmap_keyb_sphinx_help" => "Write_Mouse_Cmap_Keyb_Sphinx_Help",
        _ => return None,
    })
}

fn parse_data_command(name: &str, args: &[String], working_dir: &Path) -> Result<Step> {
    let specs = [
        ("surf_label", "SO_label", Arity::One),
        ("label", "SO_label", Arity::One),
        ("load_dset", "Dset_FileName", Arity::Path),
        ("load_col", "Col_FileName", Arity::Path),
        ("dset_label", "dset_label", Arity::One),
    ];
    Ok(Step::Remote(RemoteCommand {
        name: name.into(),
        attributes: parse_options(args, &specs, working_dir)?,
    }))
}

#[derive(Debug, Clone, Copy)]
enum Arity {
    One,
    Path,
}

fn parse_options(
    args: &[String],
    specs: &[(&str, &str, Arity)],
    working_dir: &Path,
) -> Result<BTreeMap<String, String>> {
    let mut attributes = BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        let raw = args[index].clone();
        let option = option_name(&raw)?;
        let Some((_, wire_name, arity)) = specs.iter().find(|(name, _, _)| *name == option) else {
            return Err(Error::Usage(format!("unsupported option {raw:?}")));
        };
        let value = match arity {
            Arity::One => required_value(args, &mut index, &raw)?,
            Arity::Path => {
                let path = PathBuf::from(required_value(args, &mut index, &raw)?);
                if path.is_absolute() {
                    path
                } else {
                    working_dir.join(path)
                }
                .to_string_lossy()
                .into_owned()
            }
        };
        attributes.insert((*wire_name).into(), value);
    }
    Ok(attributes)
}

fn parse_number(value: &str, option: &str) -> Result<f64> {
    let value = value
        .parse::<f64>()
        .map_err(|_| Error::Usage(format!("{option} requires numeric values")))?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::Usage(format!(
            "{option} requires finite numeric values"
        )))
    }
}

fn option_name(value: &str) -> Result<String> {
    value
        .strip_prefix('-')
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .ok_or_else(|| Error::Usage(format!("expected an option, found {value:?}")))
}

fn looks_like_option(value: &str) -> bool {
    value.starts_with('-') && value.parse::<f64>().is_err()
}

fn required_value(args: &[String], index: &mut usize, option: &str) -> Result<String> {
    *index += 1;
    let value = args
        .get(*index)
        .filter(|value| !looks_like_option(value))
        .cloned()
        .ok_or_else(|| Error::Usage(format!("{option} requires a value")))?;
    *index += 1;
    Ok(value)
}

fn take(args: &[String], index: &mut usize, count: usize) -> Result<Vec<String>> {
    let option = args[*index].clone();
    *index += 1;
    if args.len().saturating_sub(*index) < count {
        return Err(Error::Usage(format!("{option} requires {count} value(s)")));
    }
    let values = args[*index..*index + count].to_vec();
    *index += count;
    Ok(values)
}

#[derive(Debug)]
struct KeySpec {
    value: String,
    repeat: usize,
    pause: f64,
    redisplay: bool,
    string_value: Option<String>,
}

fn parse_key(modifiers: &str, value: String) -> Result<KeySpec> {
    let mut key = KeySpec {
        value,
        repeat: 1,
        pause: 0.0,
        redisplay: false,
        string_value: None,
    };
    for modifier in modifiers.split(':').filter(|value| !value.is_empty()) {
        if let Some(value) = modifier.strip_prefix('r') {
            key.repeat = value
                .parse()
                .map_err(|_| Error::Invalid(format!("invalid key repeat {modifier:?}")))?;
        } else if let Some(value) = modifier.strip_prefix('s') {
            key.pause = DurationArg::parse(value)?.0.as_secs_f64();
        } else if modifier == "p" {
            key.pause = -1.0;
        } else if modifier == "d" {
            key.redisplay = true;
        } else if let Some(value) = modifier.strip_prefix('v') {
            key.string_value = Some(format!("v{value}"));
        } else {
            return Err(Error::Invalid(format!("invalid key modifier {modifier:?}")));
        }
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_get_label_as_a_target_neutral_remote_command() {
        let Step::Remote(command) =
            parse_command(&["get_label".into()], Path::new("/demo")).unwrap()
        else {
            panic!("expected remote command")
        };

        assert_eq!(command.name, "get_label");
        assert!(command.attributes.is_empty());
    }

    #[test]
    fn parses_fatcat_surface_command() {
        let tokens = words(&[
            "surf_cont",
            "-surf_label",
            "Net_000.gii",
            "-load_dset",
            "Net_000.cols.niml.dset",
            "-switch_cmap",
            "ROI_i32",
            "-Dim",
            "0.3",
        ]);
        let Step::Remote(command) = parse_command(&tokens, Path::new("/demo")).unwrap() else {
            panic!("expected remote command")
        };
        assert_eq!(command.name, "surf_cont");
        assert_eq!(command.attributes["SO_label"], "Net_000.gii");
        assert_eq!(
            command.attributes["Dset_FileName"],
            "/demo/Net_000.cols.niml.dset"
        );
        assert_eq!(command.attributes["switch_cmap"], "ROI_i32");
        assert_eq!(command.attributes["Dim"], "0.3");
    }

    #[test]
    fn parses_keys_and_window_geometry() {
        let tokens = words(&[
            "viewer_cont",
            "-viewer_size",
            "900",
            "1000",
            "-key:r3:d",
            "right",
            "-key:v54R",
            "j",
        ]);
        let Step::Remote(command) = parse_command(&tokens, Path::new("/demo")).unwrap() else {
            panic!("expected remote command")
        };
        assert_eq!(command.attributes["WindWidth"], "900");
        assert_eq!(command.attributes["WindHeight"], "1000");
        assert_eq!(command.attributes["Key_rep_0"], "3");
        assert_eq!(command.attributes["Key_redis_0"], "1");
        assert_eq!(command.attributes["Key_strval_1"], "v54R");
    }

    #[test]
    fn key_sleep_and_pause_match_legacy_wire_values() {
        let timed = words(&["viewer_cont", "-key:r3:s0.2", "j"]);
        let Step::Remote(timed) = parse_command(&timed, Path::new("/demo")).unwrap() else {
            panic!("expected remote command")
        };
        assert_eq!(timed.attributes["Key_rep_0"], "3");
        assert_eq!(timed.attributes["Key_pause_0"], "0.200000");
        assert_eq!(timed.attributes["Key_redis_0"], "0");

        let paused = words(&["viewer_cont", "-key:p", "j"]);
        let Step::Remote(paused) = parse_command(&paused, Path::new("/demo")).unwrap() else {
            panic!("expected remote command")
        };
        assert_eq!(paused.attributes["Key_pause_0"], "-1.000000");
    }

    #[test]
    fn parses_legacy_common_modes_environment_and_aliases() {
        let tokens = words(&[
            "viewer_cont",
            "-setSUMAenv",
            "'SUMA_BackgroundColor = black'",
            "-setSUMAenv",
            "SUMA_NumForeSmoothing = 4",
            "-RenderMode",
            "fill",
            "-TransMode",
            "50%",
            "-SET_FUNC_ALPHA",
            "L",
            "-SET_FUNC_BOXED",
            "on",
            "-UseClust",
            "yes",
            "-show_0",
            "no",
        ]);
        let Step::Remote(command) = parse_command(&tokens, Path::new("/demo")).unwrap() else {
            panic!("expected remote command")
        };
        assert_eq!(command.attributes["ENV.0"], "SUMA_BackgroundColor = black");
        assert_eq!(command.attributes["ENV.1"], "SUMA_NumForeSmoothing = 4");
        assert_eq!(command.attributes["view_surf"], "Fill");
        assert_eq!(command.attributes["trans_surf"], "8");
        assert_eq!(command.attributes["SET_FUNC_ALPHA"], "y");
        assert_eq!(command.attributes["SET_FUNC_ALPHA_MODE"], "L");
        assert_eq!(command.attributes["SET_FUNC_BOXED"], "y");
        assert_eq!(command.attributes["UseClst"], "y");
        assert_eq!(command.attributes["shw_0"], "n");
    }

    #[test]
    fn common_options_are_available_to_each_controller_action() {
        let tokens = words(&[
            "surf_cont",
            "-masks",
            "-write_tract_cont_help",
            "tract-help.txt",
            "-snap_roi_cont_widgets",
            "roi.png",
            "-save_last_n",
            "3",
            "-viewer_position",
            "120",
            "240",
        ]);
        let Step::Remote(command) = parse_command(&tokens, Path::new("/demo")).unwrap() else {
            panic!("expected remote command")
        };
        assert_eq!(command.attributes["Masks"], "Click!");
        assert_eq!(
            command.attributes["Write_Tract_Cont_Help"],
            "tract-help.txt"
        );
        assert_eq!(command.attributes["Snap_ROI_Cont_Widgets"], "roi.png");
        assert_eq!(command.attributes["Save_From"], "-3");
        assert_eq!(command.attributes["Save_To"], "0");
        assert_eq!(command.attributes["WindX"], "120");
        assert_eq!(command.attributes["WindY"], "240");
        assert_eq!(command.attributes["DoViewerSetup"], "y");
    }

    #[test]
    fn builds_and_validates_inline_display_objects() {
        let tokens = words(&[
            "viewer_cont",
            "-fixed_do2",
            "<T text='Hi' coord='0.5 0.2 0'/>",
        ]);
        let Step::Remote(command) = parse_command(&tokens, Path::new("/demo")).unwrap() else {
            panic!("expected remote command")
        };
        let display_object = &command.attributes["DO_FileName"];
        assert!(display_object.contains("coord_type = 'fixed'"));
        assert!(display_object.contains("idcode_str = RS_"));
        assert!(display_object.ends_with("<T text='Hi' coord='0.5 0.2 0'/>"));

        let invalid = words(&["viewer_cont", "-mobile_do", "not NIML"]);
        assert!(parse_command(&invalid, Path::new("/demo")).is_err());
    }

    #[test]
    fn validates_legacy_controller_ranges() {
        let invalid_size = words(&["viewer_cont", "-viewer_size", "5", "900"]);
        assert!(parse_command(&invalid_size, Path::new("/demo")).is_err());

        let invalid_smoothing = words(&["viewer_cont", "-N_foreg_smooth", "501"]);
        assert!(parse_command(&invalid_smoothing, Path::new("/demo")).is_err());

        let conflicting_surface =
            words(&["surf_cont", "-surf_label", "left", "-switch_surf", "right"]);
        assert!(parse_command(&conflicting_surface, Path::new("/demo")).is_err());
    }

    #[test]
    fn maps_legacy_surface_switches_to_shared_readers() {
        let cases = [
            ("i_gii", "mesh.gii", SurfaceFormat::Gifti),
            ("i_ply", "mesh.data", SurfaceFormat::Ply),
            ("i_stl", "mesh.data", SurfaceFormat::Stl),
            ("i_obj", "mesh.data", SurfaceFormat::WavefrontObj),
            ("i_byu", "mesh.data", SurfaceFormat::Byu),
            ("i_dx", "mesh.data", SurfaceFormat::OpenDx),
            ("i_iv", "mesh.data", SurfaceFormat::OpenInventor),
            ("i_mni", "mesh.data", SurfaceFormat::MniObj),
            ("i_bv", "mesh.data", SurfaceFormat::BrainVoyager),
        ];
        for (option, path, expected) in cases {
            assert_eq!(
                format_for_option(option, Path::new(path)).unwrap(),
                expected
            );
        }
        assert_eq!(
            format_for_option("i_fs", Path::new("lh.white.asc")).unwrap(),
            SurfaceFormat::FreeSurferAscii
        );
    }

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }
}
