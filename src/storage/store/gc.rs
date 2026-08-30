#![allow(
	clippy::wildcard_imports,
	reason = "move-only module split keeps the existing imports during stage 1"
)]

use super::*;

pub(super) fn remove_unreachable_graph_generations(
	root: &Path,
	files: &FileCatalog,
) -> FerrosGrynnResult<()> {
	let referenced = files
		.files
		.iter()
		.filter(|entry| entry.graph_id != GraphId::SYSTEM)
		.filter_map(|entry| entry.path.resolve(root).parent().map(Path::to_path_buf))
		.collect::<BTreeSet<_>>();
	let graphs = root.join("graphs");
	let graph_directories = match fs::read_dir(&graphs) {
		Ok(entries) => entries,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
		Err(source) => return Err(storage_io("enumerate graph directories", source)),
	};
	for graph_entry in graph_directories {
		let graph_entry =
			graph_entry.map_err(|source| storage_io("read graph directory entry", source))?;
		let graph_name = graph_entry.file_name();
		let Some(graph_name) = graph_name.to_str() else {
			continue;
		};
		if graph_name.len() != 16
			|| !graph_name.bytes().all(|byte| byte.is_ascii_hexdigit())
			|| !graph_entry
				.file_type()
				.map_err(|source| storage_io("inspect graph directory", source))?
				.is_dir()
		{
			continue;
		}
		let graph_path = graph_entry.path();
		for generation_entry in fs::read_dir(&graph_path)
			.map_err(|source| storage_io("enumerate graph generations", source))?
		{
			let generation_entry = generation_entry
				.map_err(|source| storage_io("read graph generation entry", source))?;
			let generation_name = generation_entry.file_name();
			let Some(generation_name) = generation_name.to_str() else {
				continue;
			};
			if generation_name.parse::<Uuid>().is_err()
				|| !generation_entry
					.file_type()
					.map_err(|source| storage_io("inspect graph generation", source))?
					.is_dir() || referenced.contains(&generation_entry.path())
			{
				continue;
			}
			fs::remove_dir_all(generation_entry.path())
				.map_err(|source| storage_io("remove unreachable graph generation", source))?;
		}
		let mut remaining = fs::read_dir(&graph_path)
			.map_err(|source| storage_io("inspect graph directory after collection", source))?;
		if remaining.next().is_none() {
			fs::remove_dir(&graph_path)
				.map_err(|source| storage_io("remove empty graph directory", source))?;
		}
	}
	Ok(())
}
