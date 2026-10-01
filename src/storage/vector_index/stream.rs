//! Page-at-a-time vector base generation encoding.

use std::borrow::Borrow;

use super::codec::{
	StreamLocation,
	VectorBuildSpec,
	VectorNode,
	encode_header,
	encode_vector_node_header,
	encode_vector_payload,
	validate_nodes,
	vector_node_record_length,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		direct::DirectFile,
		graph_index::StreamedIndex,
		immutable_stream::{ImmutableStreamLocation, ImmutableStreamWriter},
		page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType},
	},
};

/// Writes one complete HNSW or DiskANN base without retaining encoded pages.
pub(in crate::storage) fn write_vector_index_stream<V: Borrow<crate::values::Vector>>(
	file: &DirectFile,
	spec: VectorBuildSpec,
	entrypoint: Option<u32>,
	nodes: &[VectorNode<V>],
) -> FerrosGrynnResult<StreamedIndex> {
	if file.page_count() != 0 {
		return Err(invalid("vector streaming target is not empty"));
	}
	validate_nodes(spec, entrypoint, nodes)?;
	let posting_count = nodes.iter().try_fold(0_usize, |total, node| {
		total
			.checked_add(node.postings.len())
			.ok_or_else(|| invalid("vector posting count overflows usize"))
	})?;
	let mut header = Box::new(AlignedPage::zeroed());
	header.reset(PageType::IndexVector, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
	header.seal()?;
	file.write_page(PageNo::new(0), &header)?;

	let mut data = ImmutableStreamWriter::new(file, PageType::IndexVectorData);
	for node in nodes {
		let vector = encode_vector_payload(node)?;
		let record_length = vector_node_record_length(node, vector.len())?;
		data.write_all(&encode_vector_node_header(node, vector.len(), record_length)?)?;
		for atom in &node.postings {
			data.write_all(&atom.raw().to_le_bytes())?;
		}
		for neighbors in &node.neighbors {
			data.write_all(
				&u32::try_from(neighbors.len())
					.map_err(|_error| invalid("vector neighbor count exceeds u32"))?
					.to_le_bytes(),
			)?;
			for neighbor in neighbors {
				data.write_all(&neighbor.to_le_bytes())?;
			}
		}
		data.write_all(&vector)?;
	}
	let data = data.finish()?;

	let mut directory = ImmutableStreamWriter::new(file, PageType::IndexVectorData);
	let mut offset = 0_u64;
	for node in nodes {
		let vector_length = encode_vector_payload(node)?.len();
		let record_length = vector_node_record_length(node, vector_length)?;
		directory.write_all(&offset.to_le_bytes())?;
		directory.write_all(
			&u32::try_from(record_length)
				.map_err(|_error| invalid("vector record length exceeds u32"))?
				.to_le_bytes(),
		)?;
		directory.write_all(&0_u32.to_le_bytes())?;
		offset = offset
			.checked_add(
				u64::try_from(record_length)
					.map_err(|_error| invalid("vector record length exceeds u64"))?,
			)
			.ok_or_else(|| invalid("vector record offsets overflow u64"))?;
	}
	let directory = directory.finish()?;
	if offset != data.bytes {
		return Err(invalid("vector directory does not consume its data stream"));
	}
	encode_header(
		&mut header,
		spec,
		entrypoint,
		nodes.len(),
		posting_count,
		location(directory),
		location(data),
	)?;
	file.write_page(PageNo::new(0), &header)?;
	Ok(StreamedIndex {
		root: PageNo::new(0),
		page_count: file.page_count(),
	})
}

fn location(location: ImmutableStreamLocation) -> StreamLocation {
	StreamLocation {
		first: location.first,
		pages: location.pages,
	}
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use std::{
		fs,
		num::NonZeroU16,
		sync::atomic::{AtomicU64, Ordering},
	};

	use super::write_vector_index_stream;
	use crate::{
		datastore::{
			ClassId,
			ClassIndexOptions,
			DiskAnnIndexOptions,
			IndexId,
			IndexScope,
			SchemaVersion,
			VectorDistance,
		},
		graph::AtomKind,
		storage::{
			direct::DirectFile,
			page::{AlignedPage, PageNo},
			vector_index::{
				VectorBuildSpec,
				VectorDocument,
				build_vector_graph_with_distance,
				decode_vector_index,
			},
		},
		values::{PropertyKeyId, Vector, VectorKind, storage::DiskAtomRef},
	};

	static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

	struct TestFile(std::path::PathBuf);

	impl TestFile {
		fn new() -> Self {
			Self(std::env::temp_dir().join(format!(
				"fg-vector-stream-{:016x}.fgidx",
				NEXT_FILE.fetch_add(1, Ordering::Relaxed)
			)))
		}
	}

	impl Drop for TestFile {
		fn drop(&mut self) {
			drop(fs::remove_file(&self.0));
		}
	}

	#[test]
	fn streamed_vector_file_round_trips_without_retaining_encoded_pages() {
		let options = DiskAnnIndexOptions::defaults(
			VectorKind::F32,
			NonZeroU16::new(2).unwrap(),
			VectorDistance::Euclidean,
			true,
		)
		.unwrap();
		let spec = VectorBuildSpec {
			index: IndexId::new(7).unwrap(),
			index_version: SchemaVersion::INITIAL,
			scope: IndexScope::Base,
			class: ClassId::new(3).unwrap(),
			field: PropertyKeyId::new(2),
			options: ClassIndexOptions::DiskAnn(options),
			build_revision: 11,
			build_seed: 19,
		};
		let documents = (1..=96_u64)
			.map(|slot| VectorDocument {
				atom: DiskAtomRef::from_parts(AtomKind::Node, slot).unwrap(),
				vector: Vector::from(vec![
					f32::from(u16::try_from(slot).unwrap()),
					f32::from(u16::try_from(slot % 7).unwrap()),
				]),
			})
			.collect();
		let mut unavailable = |_function, _version, _left: &Vector, _right: &Vector| {
			unreachable!("built-in distance must not invoke a UDF")
		};
		let (entrypoint, nodes) =
			build_vector_graph_with_distance(spec, documents, &mut unavailable).unwrap();
		let path = TestFile::new();
		let file = DirectFile::create_new(&path.0).unwrap();
		let built = write_vector_index_stream(&file, spec, entrypoint, &nodes).unwrap();
		assert_eq!(built.page_count, file.page_count());

		let mut pages = Vec::with_capacity(usize::try_from(file.page_count()).unwrap());
		for page_no in 0..file.page_count() {
			pages.push(AlignedPage::zeroed());
			file.read_page(PageNo::new(page_no), pages.last_mut().unwrap()).unwrap();
		}
		let decoded = decode_vector_index(&pages).unwrap();
		assert_eq!(decoded.spec, spec);
		assert_eq!(decoded.entrypoint, entrypoint);
		assert_eq!(decoded.nodes, nodes);
	}
}
