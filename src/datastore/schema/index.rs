use std::{
	cmp::Ordering,
	collections::BTreeSet,
	hash::{Hash, Hasher},
	num::NonZeroU16,
};

use super::{AnalyzerId, FunctionId, IndexId, SchemaVersion};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{GeometryKind, GraphId, Text, VectorKind},
};

/// Distance contract shared by exact KNN and durable ANN definitions.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum VectorDistance {
	/// Square root of the sum of squared component differences.
	Euclidean,
	/// One minus cosine similarity.
	Cosine,
	/// Sum of absolute component differences.
	Manhattan,
	/// Minkowski distance with the supplied exponent.
	Minkowski(
		/// Minkowski distance with the supplied exponent.
		f64,
	),
	/// Negative dot product, so lower remains a better distance.
	InnerProduct,
	/// Cosine distance after normalizing a derived search copy.
	CosineNormalized,
	/// Exact durable PURE COST function identity and version.
	Custom {
		/// Function identity.
		function: FunctionId,
		/// Function definition version captured by the index.
		version: SchemaVersion,
	},
}

impl VectorDistance {
	/// Validates parameters shared by definitions and exact search.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error for a non-finite or sub-unit
	/// Minkowski exponent.
	pub fn validate(self) -> FerrosGrynnResult<()> {
		if let Self::Minkowski(exponent) = self
			&& (!exponent.is_finite() || exponent < 1.0)
		{
			return Err(FerrosGrynnError::invalid_argument(
				"Minkowski exponent must be finite and at least one",
			));
		}
		Ok(())
	}

	/// Returns the rank result.
	fn rank(self) -> u8 {
		match self {
			Self::Euclidean => 0,
			Self::Cosine => 1,
			Self::Manhattan => 2,
			Self::Minkowski(_) => 3,
			Self::InnerProduct => 4,
			Self::CosineNormalized => 5,
			Self::Custom {
				..
			} => 6,
		}
	}
}

impl PartialEq for VectorDistance {
	/// Compares this `VectorDistance` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Eq for VectorDistance {}

impl PartialOrd for VectorDistance {
	/// Compares this `VectorDistance` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for VectorDistance {
	/// Compares this `VectorDistance` with another value using its ordering
	/// contract.
	fn cmp(&self, other: &Self) -> Ordering {
		self.rank().cmp(&other.rank()).then_with(|| match (*self, *other) {
			(Self::Minkowski(left), Self::Minkowski(right)) => left.total_cmp(&right),
			(
				Self::Custom {
					function: left_function,
					version: left_version,
				},
				Self::Custom {
					function: right_function,
					version: right_version,
				},
			) => (left_function, left_version).cmp(&(right_function, right_version)),
			_ => Ordering::Equal,
		})
	}
}

impl Hash for VectorDistance {
	/// Feeds this `VectorDistance` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.rank().hash(state);
		match *self {
			Self::Minkowski(exponent) => exponent.to_bits().hash(state),
			Self::Custom {
				function,
				version,
			} => {
				function.hash(state);
				version.hash(state);
			}
			_ => {}
		}
	}
}

/// Fulltext-specific definition payload stored by one class index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FulltextIndexOptions {
	/// Analyzer associated with this fulltext index options.
	pub(crate) analyzer: AnalyzerId,
	/// Analyzer version associated with this fulltext index options.
	pub(crate) analyzer_version: SchemaVersion,
	/// K1 bits associated with this fulltext index options.
	pub(crate) k1_bits: u64,
	/// B bits associated with this fulltext index options.
	pub(crate) b_bits: u64,
	/// Highlights associated with this fulltext index options.
	pub(crate) highlights: bool,
}

impl FulltextIndexOptions {
	/// Creates a checked BM25 definition.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error unless `k1` is finite and positive and
	/// `b` is finite and in the inclusive range zero through one.
	pub fn new(
		analyzer: AnalyzerId,
		analyzer_version: SchemaVersion,
		k1: f64,
		b: f64,
		highlights: bool,
	) -> FerrosGrynnResult<Self> {
		if !k1.is_finite() || k1 <= 0.0 || !b.is_finite() || !(0.0..=1.0).contains(&b) {
			return Err(FerrosGrynnError::invalid_argument(
				"BM25 requires finite k1 > 0 and b in 0..=1",
			));
		}
		Ok(Self {
			analyzer,
			analyzer_version,
			k1_bits: k1.to_bits(),
			b_bits: b.to_bits(),
			highlights,
		})
	}

	/// Creates the default BM25(1.2, 0.75) definition.
	pub fn defaults(
		analyzer: AnalyzerId,
		analyzer_version: SchemaVersion,
		highlights: bool,
	) -> Self {
		Self {
			analyzer,
			analyzer_version,
			k1_bits: 1.2_f64.to_bits(),
			b_bits: 0.75_f64.to_bits(),
			highlights,
		}
	}

	/// Returns the stored analyzer of this `FulltextIndexOptions`.
	pub const fn analyzer(self) -> AnalyzerId {
		self.analyzer
	}

	/// Returns the stored analyzer version of this `FulltextIndexOptions`.
	pub const fn analyzer_version(self) -> SchemaVersion {
		self.analyzer_version
	}

	/// Returns k1 for `FulltextIndexOptions`.
	pub fn k1(self) -> f64 {
		f64::from_bits(self.k1_bits)
	}

	/// Returns b for `FulltextIndexOptions`.
	pub fn b(self) -> f64 {
		f64::from_bits(self.b_bits)
	}

	/// Returns the stored highlights of this `FulltextIndexOptions`.
	pub const fn highlights(self) -> bool {
		self.highlights
	}
}

/// Durable HNSW construction and search contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HnswIndexOptions {
	/// Element associated with this hnsw index options.
	pub(crate) element: VectorKind,
	/// Dimension associated with this hnsw index options.
	pub(crate) dimension: NonZeroU16,
	/// Distance associated with this hnsw index options.
	pub(crate) distance: VectorDistance,
	/// M associated with this hnsw index options.
	pub(crate) m: NonZeroU16,
	/// Ef construction associated with this hnsw index options.
	pub(crate) ef_construction: NonZeroU16,
	/// M0 associated with this hnsw index options.
	pub(crate) m0: NonZeroU16,
}

impl HnswIndexOptions {
	/// Creates a checked HNSW definition.
	///
	/// # Errors
	///
	/// Returns an error for an unsupported element type or distance, or when
	/// `M0 = 2M` does not fit the durable representation.
	pub fn new(
		element: VectorKind,
		dimension: NonZeroU16,
		distance: VectorDistance,
		m: NonZeroU16,
		ef_construction: NonZeroU16,
	) -> FerrosGrynnResult<Self> {
		distance.validate()?;
		if !matches!(
			element,
			VectorKind::F64 | VectorKind::F32 | VectorKind::I64 | VectorKind::I32 | VectorKind::I16
		) || matches!(distance, VectorDistance::InnerProduct | VectorDistance::CosineNormalized)
		{
			return Err(FerrosGrynnError::invalid_argument(
				"HNSW element type or distance is unsupported",
			));
		}
		let m0 =
			m.get().checked_mul(2).and_then(NonZeroU16::new).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("HNSW M is too large for M0=2M")
			})?;
		Ok(Self {
			element,
			dimension,
			distance,
			m,
			ef_construction,
			m0,
		})
	}

	/// Creates the default HNSW definition for an exact vector type and
	/// dimension.
	///
	/// # Errors
	///
	/// Returns an error for an unsupported element type or distance.
	pub fn defaults(
		element: VectorKind,
		dimension: NonZeroU16,
		distance: VectorDistance,
	) -> FerrosGrynnResult<Self> {
		Self::new(
			element,
			dimension,
			distance,
			NonZeroU16::new(12).ok_or_else(invalid_ann_defaults)?,
			NonZeroU16::new(150).ok_or_else(invalid_ann_defaults)?,
		)
	}

	/// Returns the stored element of this `HnswIndexOptions`.
	pub const fn element(self) -> VectorKind {
		self.element
	}

	/// Returns the stored dimension of this `HnswIndexOptions`.
	pub const fn dimension(self) -> NonZeroU16 {
		self.dimension
	}

	/// Returns the stored distance of this `HnswIndexOptions`.
	pub const fn distance(self) -> VectorDistance {
		self.distance
	}

	/// Returns the stored m of this `HnswIndexOptions`.
	pub const fn m(self) -> NonZeroU16 {
		self.m
	}

	/// Returns the stored ef construction of this `HnswIndexOptions`.
	pub const fn ef_construction(self) -> NonZeroU16 {
		self.ef_construction
	}

	/// Returns the stored m0 of this `HnswIndexOptions`.
	pub const fn m0(self) -> NonZeroU16 {
		self.m0
	}

	/// Returns the default query breadth for this vector dimension and `K`.
	pub(crate) fn default_search_breadth(self, k: usize) -> usize {
		k.max(1_024).max(usize::from(self.dimension.get()))
	}
}

/// Durable page-backed DiskANN construction and search contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiskAnnIndexOptions {
	/// Element associated with this disk ann index options.
	pub(crate) element: VectorKind,
	/// Dimension associated with this disk ann index options.
	pub(crate) dimension: NonZeroU16,
	/// Distance associated with this disk ann index options.
	pub(crate) distance: VectorDistance,
	/// Degree associated with this disk ann index options.
	pub(crate) degree: NonZeroU16,
	/// Build list size associated with this disk ann index options.
	pub(crate) build_list_size: NonZeroU16,
	/// Alpha bits associated with this disk ann index options.
	pub(crate) alpha_bits: u32,
	/// Hashed vector associated with this disk ann index options.
	pub(crate) hashed_vector: bool,
}

impl DiskAnnIndexOptions {
	/// Creates a checked DiskANN definition.
	///
	/// # Errors
	///
	/// Returns an error for an unsupported element type or distance, or for a
	/// non-finite/sub-unit pruning alpha.
	pub fn new(
		element: VectorKind,
		dimension: NonZeroU16,
		distance: VectorDistance,
		degree: NonZeroU16,
		build_list_size: NonZeroU16,
		alpha: f32,
		hashed_vector: bool,
	) -> FerrosGrynnResult<Self> {
		distance.validate()?;
		if !matches!(element, VectorKind::F32 | VectorKind::F16 | VectorKind::I8 | VectorKind::U8)
			|| !matches!(
				distance,
				VectorDistance::Euclidean
					| VectorDistance::Cosine
					| VectorDistance::InnerProduct
					| VectorDistance::CosineNormalized
			) {
			return Err(FerrosGrynnError::invalid_argument(
				"DiskANN element type or distance is unsupported",
			));
		}
		if !alpha.is_finite() || alpha < 1.0 {
			return Err(FerrosGrynnError::invalid_argument(
				"DiskANN alpha must be finite and at least one",
			));
		}
		if distance == VectorDistance::CosineNormalized
			&& !matches!(element, VectorKind::F32 | VectorKind::F16)
		{
			return Err(FerrosGrynnError::invalid_argument(
				"COSINE_NORMALIZED requires F32 or F16 DiskANN vectors",
			));
		}
		Ok(Self {
			element,
			dimension,
			distance,
			degree,
			build_list_size,
			alpha_bits: alpha.to_bits(),
			hashed_vector,
		})
	}

	/// Creates the default DiskANN definition.
	///
	/// # Errors
	///
	/// Returns an error for an unsupported element type, distance, or custom
	/// distance function.
	pub fn defaults(
		element: VectorKind,
		dimension: NonZeroU16,
		distance: VectorDistance,
		hashed_vector: bool,
	) -> FerrosGrynnResult<Self> {
		Self::new(
			element,
			dimension,
			distance,
			NonZeroU16::new(64).ok_or_else(invalid_ann_defaults)?,
			NonZeroU16::new(100).ok_or_else(invalid_ann_defaults)?,
			1.2,
			hashed_vector,
		)
	}

	/// Returns the stored element of this `DiskAnnIndexOptions`.
	pub const fn element(self) -> VectorKind {
		self.element
	}

	/// Returns the stored dimension of this `DiskAnnIndexOptions`.
	pub const fn dimension(self) -> NonZeroU16 {
		self.dimension
	}

	/// Returns the stored distance of this `DiskAnnIndexOptions`.
	pub const fn distance(self) -> VectorDistance {
		self.distance
	}

	/// Returns the stored degree of this `DiskAnnIndexOptions`.
	pub const fn degree(self) -> NonZeroU16 {
		self.degree
	}

	/// Returns the stored build list size of this `DiskAnnIndexOptions`.
	pub const fn build_list_size(self) -> NonZeroU16 {
		self.build_list_size
	}

	/// Returns alpha for `DiskAnnIndexOptions`.
	pub fn alpha(self) -> f32 {
		f32::from_bits(self.alpha_bits)
	}

	/// Returns the stored hashed vector of this `DiskAnnIndexOptions`.
	pub const fn hashed_vector(self) -> bool {
		self.hashed_vector
	}
}

/// Returns the invalid ann defaults result.
fn invalid_ann_defaults() -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument("internal ANN defaults contain zero")
}

/// Logical scope of one user-declared class index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum IndexScope {
	/// Indexes base values of every global atom in the class.
	Base,
	/// Indexes effective values of memberships in one graph.
	Effective {
		/// Graph whose inherited and overridden values are indexed.
		graph: GraphId,
	},
}

/// Physical contract for one user-declared class index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClassIndexKind {
	/// One key may reference multiple atoms.
	Standard,
	/// One key may reference at most one atom.
	Unique,
	/// Exact number of live members; no fields participate in the key.
	Count,
	/// BM25 inverted index over one string field.
	Fulltext,
	/// In-memory navigable small-world vector index.
	Hnsw,
	/// Page-backed DiskANN vector index.
	DiskAnn,
	/// Exact STR R-tree candidates over one geometry field.
	Geometry,
}

/// Geometry definition options; topology, coordinate model and fill are fixed
/// by stage 24.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GeometryIndexOptions {
	/// Optional exact geometry family; None accepts all seven families.
	pub(crate) geometry_type: Option<GeometryKind>,
}

impl GeometryIndexOptions {
	/// Creates options from an already validated geometry family.
	pub const fn new(geometry_type: Option<GeometryKind>) -> Self {
		Self {
			geometry_type,
		}
	}

	/// Returns the declared family restriction.
	pub const fn geometry_type(self) -> Option<GeometryKind> {
		self.geometry_type
	}

	/// Validates geometry and the optional TYPE restriction at build/write
	/// boundaries.
	pub(crate) fn validate(self, geometry: &crate::values::Geometry) -> FerrosGrynnResult<()> {
		geometry.validate()?;
		if self.geometry_type.is_some_and(|kind| kind != geometry.kind()) {
			return Err(FerrosGrynnError::invalid_argument(
				"geometry does not match the index TYPE restriction",
			));
		}
		Ok(())
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Class index options alternatives.
pub enum ClassIndexOptions {
	/// Scalar case of class index options.
	Scalar,
	/// Fulltext case of class index options.
	Fulltext(
		/// Fulltext case of class index options.
		FulltextIndexOptions,
	),
	/// Hnsw case of class index options.
	Hnsw(
		/// Hnsw case of class index options.
		HnswIndexOptions,
	),
	/// Disk ann case of class index options.
	DiskAnn(
		/// Disk ann case of class index options.
		DiskAnnIndexOptions,
	),
	/// Geometry case of class index options.
	Geometry(
		/// Geometry case of class index options.
		GeometryIndexOptions,
	),
}

/// Transaction-visible usability state of one declared class index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClassIndexState {
	/// The physical index is available to query planning.
	Ready,
	/// The definition is retained but queries must fall back to a scan.
	Stale,
	/// The index is deliberately excluded before a later `REMOVE INDEX`.
	PreparedForRemoval,
}

/// Write-side maintenance policy of one user-declared class index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IndexMaintenanceMode {
	/// Every committed mutation updates the index before publication.
	Immediate,
	/// Mutations mark the index stale until maintenance catches up.
	Deferred,
}

/// Declarative scalar B+tree attached to one class schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassIndex {
	/// Stable identity assigned when the containing class is published.
	pub(crate) id: Option<IndexId>,
	/// Semantic definition version.
	pub(crate) version: SchemaVersion,
	/// Class-local index name.
	pub(crate) name: Text,
	/// Physical index contract.
	pub(crate) kind: ClassIndexKind,
	/// Observable planner-availability contract.
	pub(crate) state: ClassIndexState,
	/// Write-side maintenance policy.
	pub(crate) maintenance: IndexMaintenanceMode,
	/// Commit revision that published the currently query-visible build.
	pub(crate) build_revision: u64,
	/// Explicit base or graph-effective scope.
	pub(crate) scope: IndexScope,
	/// Indexed fields in composite-key order.
	pub(crate) fields: Vec<Text>,
	/// Options associated with this class index.
	pub(crate) options: ClassIndexOptions,
}

impl ClassIndex {
	/// Creates an index definition; field existence is checked by
	/// [`super::ClassSchema::with_index`].
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when the name is empty, a Count index
	/// names fields, or a scalar index has no fields, names the synthetic `id`
	/// column, or repeats a field.
	pub fn new<IntoText, Fields>(
		name: IntoText,
		kind: ClassIndexKind,
		scope: IndexScope,
		fields: Fields,
	) -> FerrosGrynnResult<Self>
	where
		IntoText: Into<Text>,
		Fields: IntoIterator,
		Fields::Item: Into<Text>,
	{
		let name = name.into();
		let fields = fields.into_iter().map(Into::into).collect::<Vec<_>>();
		let mut unique = BTreeSet::new();
		let invalid_fields = match kind {
			ClassIndexKind::Count => !fields.is_empty(),
			ClassIndexKind::Standard | ClassIndexKind::Unique => fields.is_empty(),
			ClassIndexKind::Fulltext
			| ClassIndexKind::Hnsw
			| ClassIndexKind::DiskAnn
			| ClassIndexKind::Geometry => true,
		};
		if name.is_empty()
			|| name.as_str().contains('\0')
			|| invalid_fields
			|| fields.iter().any(|field| {
				field.is_empty()
					|| field.as_str().contains('\0')
					|| field.as_str() == "id"
					|| !unique.insert(field.clone())
			}) {
			return Err(FerrosGrynnError::invalid_argument(
				"an index requires a non-empty name and fields matching its physical kind",
			));
		}
		Ok(Self {
			id: None,
			version: SchemaVersion::INITIAL,
			name,
			kind,
			state: ClassIndexState::Ready,
			maintenance: IndexMaintenanceMode::Immediate,
			build_revision: 0,
			scope,
			fields,
			options: ClassIndexOptions::Scalar,
		})
	}

	/// Creates a Fulltext definition over exactly one string field.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn new_fulltext<IntoText: Into<Text>, IntoField: Into<Text>>(
		name: IntoText,
		scope: IndexScope,
		field: IntoField,
		options: FulltextIndexOptions,
	) -> FerrosGrynnResult<Self> {
		Self::new_specialized(
			name.into(),
			ClassIndexKind::Fulltext,
			scope,
			field.into(),
			ClassIndexOptions::Fulltext(options),
		)
	}

	/// Creates an HNSW definition over exactly one vector field.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn new_hnsw<IntoText: Into<Text>, IntoField: Into<Text>>(
		name: IntoText,
		scope: IndexScope,
		field: IntoField,
		options: HnswIndexOptions,
	) -> FerrosGrynnResult<Self> {
		Self::new_specialized(
			name.into(),
			ClassIndexKind::Hnsw,
			scope,
			field.into(),
			ClassIndexOptions::Hnsw(options),
		)
	}

	/// Creates a DiskANN definition over exactly one vector field.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn new_disk_ann<IntoText: Into<Text>, IntoField: Into<Text>>(
		name: IntoText,
		scope: IndexScope,
		field: IntoField,
		options: DiskAnnIndexOptions,
	) -> FerrosGrynnResult<Self> {
		Self::new_specialized(
			name.into(),
			ClassIndexKind::DiskAnn,
			scope,
			field.into(),
			ClassIndexOptions::DiskAnn(options),
		)
	}

	/// Returns the new specialized result.
	fn new_specialized(
		name: Text,
		kind: ClassIndexKind,
		scope: IndexScope,
		field: Text,
		options: ClassIndexOptions,
	) -> FerrosGrynnResult<Self> {
		if name.is_empty()
			|| name.as_str().contains('\0')
			|| field.is_empty()
			|| field.as_str().contains('\0')
			|| field.as_str() == "id"
		{
			return Err(FerrosGrynnError::invalid_argument(
				"a specialized index requires a non-empty name and one non-reserved field",
			));
		}
		Ok(Self {
			id: None,
			version: SchemaVersion::INITIAL,
			name,
			kind,
			state: ClassIndexState::Ready,
			maintenance: IndexMaintenanceMode::Immediate,
			build_revision: 0,
			scope,
			fields: vec![field],
			options,
		})
	}

	/// Creates an STR geometry index over exactly one geometry field.
	///
	/// # Errors
	/// Rejects empty names or reserved fields; field types are checked by
	/// ClassSchema.
	pub fn new_geometry<IntoText: Into<Text>, IntoField: Into<Text>>(
		name: IntoText,
		scope: IndexScope,
		field: IntoField,
		options: GeometryIndexOptions,
	) -> FerrosGrynnResult<Self> {
		Self::new_specialized(
			name.into(),
			ClassIndexKind::Geometry,
			scope,
			field.into(),
			ClassIndexOptions::Geometry(options),
		)
	}

	/// Returns the geometry-specific definition options.
	pub const fn geometry_options(&self) -> Option<GeometryIndexOptions> {
		match self.options {
			ClassIndexOptions::Geometry(options) => Some(options),
			_ => None,
		}
	}

	/// Selects the write-side maintenance policy.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when deferred maintenance is requested
	/// for a Unique index, whose constraint must remain immediately visible.
	pub fn with_maintenance_mode(
		mut self,
		maintenance: IndexMaintenanceMode,
	) -> FerrosGrynnResult<Self> {
		if maintenance == IndexMaintenanceMode::Deferred && self.kind == ClassIndexKind::Unique {
			return Err(FerrosGrynnError::invalid_argument(
				"deferred maintenance is not supported for Unique indexes",
			));
		}
		self.maintenance = maintenance;
		Ok(self)
	}

	/// Selects the query-visible state used while a concurrent builder owns the
	/// first physical generation.
	pub(crate) fn with_state(mut self, state: ClassIndexState) -> Self {
		self.state = state;
		self
	}

	/// Returns the stable identity after the containing class is published.
	pub const fn id(&self) -> Option<IndexId> {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the class-local index name.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the physical index contract.
	pub const fn kind(&self) -> ClassIndexKind {
		self.kind
	}

	/// Returns whether the index is usable, stale, or prepared for removal.
	pub const fn state(&self) -> ClassIndexState {
		self.state
	}

	/// Returns the write-side maintenance policy.
	pub const fn maintenance_mode(&self) -> IndexMaintenanceMode {
		self.maintenance
	}

	/// Returns the commit revision of the last successfully published build.
	/// Zero means that a concurrent initial build has not published yet.
	pub const fn build_revision(&self) -> u64 {
		self.build_revision
	}

	/// Returns the explicit base or graph-effective scope.
	pub const fn scope(&self) -> IndexScope {
		self.scope
	}

	/// Returns fields in composite-key order.
	pub fn fields(&self) -> impl ExactSizeIterator<Item = &Text> {
		self.fields.iter()
	}

	/// Returns Fulltext-specific options when this is a Fulltext index.
	pub const fn fulltext_options(&self) -> Option<FulltextIndexOptions> {
		match self.options {
			ClassIndexOptions::Fulltext(options) => Some(options),
			_ => None,
		}
	}

	/// Returns HNSW-specific options when this is an HNSW index.
	pub const fn hnsw_options(&self) -> Option<HnswIndexOptions> {
		match self.options {
			ClassIndexOptions::Hnsw(options) => Some(options),
			_ => None,
		}
	}

	/// Returns DiskANN-specific options when this is a DiskANN index.
	pub const fn disk_ann_options(&self) -> Option<DiskAnnIndexOptions> {
		match self.options {
			ClassIndexOptions::DiskAnn(options) => Some(options),
			_ => None,
		}
	}
}
