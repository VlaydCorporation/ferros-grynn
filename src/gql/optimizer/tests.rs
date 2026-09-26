use std::hash::{Hash, Hasher};

use super::{
	CanonicalHasher,
	Cardinality,
	PhysicalPlan,
	PhysicalSelectPlan,
	PhysicalSource,
	PlanCost,
	SourceOperator,
};

#[test]
fn canonical_integer_encoding_does_not_use_native_width_or_endian() {
	let mut writer = CanonicalHasher::versioned(7, b"test");
	1_usize.hash(&mut writer);
	let first = writer.finish();

	let mut writer = CanonicalHasher::versioned(7, b"test");
	writer.write_u64(1);
	let second = writer.finish();

	assert_ne!(first, second, "type tags must distinguish usize from u64");
	assert!(writer.as_bytes().starts_with(b"FGCP"));
}

#[test]
fn physical_plan_hash_is_stable_and_sensitive_to_operator_choice() {
	let source = PhysicalSource {
		node_id: 1,
		alias: "person".into(),
		column_offset: 0,
		column_width: 1,
		operator: SourceOperator::ClassScan {
			class: "person".into(),
		},
		cardinality: Cardinality::exact(3),
		cost: PlanCost {
			sequential: 3,
			..PlanCost::default()
		},
	};
	let plan = PhysicalPlan::Select(PhysicalSelectPlan {
		sources: vec![source.clone()],
		execution_order: vec![0],
		joins: Vec::new(),
		rules: Vec::new(),
		pushed_filter: false,
		pushed_limit: false,
		requires_sort: false,
		cardinality: Cardinality::exact(3),
		cost: PlanCost::default(),
		warnings: Vec::new(),
	});
	assert_eq!(plan.hash(), plan.clone().hash());
	let estimates_changed = PhysicalPlan::Select(PhysicalSelectPlan {
		sources: vec![PhysicalSource {
			cardinality: Cardinality::unknown(30),
			cost: PlanCost {
				risk: 99,
				..PlanCost::default()
			},
			..source.clone()
		}],
		execution_order: vec![0],
		joins: Vec::new(),
		rules: Vec::new(),
		pushed_filter: false,
		pushed_limit: false,
		requires_sort: false,
		cardinality: Cardinality::unknown(30),
		cost: PlanCost {
			risk: 99,
			..PlanCost::default()
		},
		warnings: vec![super::PlanWarning::StatisticsMissing("changed".into())],
	});
	assert_eq!(plan.hash(), estimates_changed.hash());

	let changed = PhysicalPlan::Select(PhysicalSelectPlan {
		sources: vec![PhysicalSource {
			operator: SourceOperator::GraphClassScan {
				class: "person".into(),
				graph: None,
			},
			..source
		}],
		execution_order: vec![0],
		joins: Vec::new(),
		rules: Vec::new(),
		pushed_filter: false,
		pushed_limit: false,
		requires_sort: false,
		cardinality: Cardinality::exact(3),
		cost: PlanCost::default(),
		warnings: Vec::new(),
	});
	assert_ne!(plan.hash(), changed.hash());
}
