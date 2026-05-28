//! Type checker coverage for records, enums, type aliases, and bidirectional
//! integer-literal inference (Phase 2 tightening).

use ol_ir::{
    Equation, Expr, NodeDef, NodeKind, Package, Port, Project, RecordField, Type, TypeBody,
    TypeDef,
};

fn project_with(node: NodeDef, types: Vec<TypeDef>) -> Project {
    Project {
        name: "tc_test".into(),
        packages: vec![Package {
            name: "p".into(),
            types,
            nodes: vec![node],
            ..Default::default()
        }],
        main: None,
    }
}

fn enum_type(name: &str, variants: &[&str]) -> TypeDef {
    TypeDef {
        body: TypeBody::Enum(ol_ir::EnumDef {
            name: name.into(),
            variants: variants.iter().map(|s| s.to_string()).collect(),
        }),
    }
}

fn record_type(name: &str, fields: &[(&str, Type)]) -> TypeDef {
    TypeDef {
        body: TypeBody::Record {
            name: name.into(),
            fields: fields
                .iter()
                .map(|(n, t)| RecordField {
                    name: n.to_string(),
                    ty: t.clone(),
                })
                .collect(),
        },
    }
}

fn alias_type(name: &str, target: Type) -> TypeDef {
    TypeDef {
        body: TypeBody::Alias {
            name: name.into(),
            target,
        },
    }
}

fn codes(report: &ol_typecheck::CheckReport) -> Vec<&str> {
    report.diagnostics.iter().map(|d| d.code.as_str()).collect()
}

#[test]
fn record_field_access_resolves_to_field_type() {
    // node Probe(msg: AdsbMsg) returns (alt: int32)
    //   alt = msg.altitude;
    let msg_type = record_type(
        "AdsbMsg",
        &[("altitude", Type::Int32), ("squawk", Type::Uint16)],
    );
    let node = NodeDef {
        name: "Probe".into(),
        kind: NodeKind::Function,
        inputs: vec![Port { name: "msg".into(), ty: Type::Named("AdsbMsg".into()) }],
        outputs: vec![Port { name: "alt".into(), ty: Type::Int32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["alt".into()],
            rhs: Expr::Field {
                base: Box::new(Expr::var("msg")),
                field: "altitude".into(),
            },
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(node, vec![msg_type]));
    assert!(
        !report.has_errors(),
        "unexpected errors: {:?}",
        codes(&report)
    );
}

#[test]
fn unknown_record_field_errors() {
    let msg_type = record_type("AdsbMsg", &[("altitude", Type::Int32)]);
    let node = NodeDef {
        name: "Probe".into(),
        kind: NodeKind::Function,
        inputs: vec![Port { name: "msg".into(), ty: Type::Named("AdsbMsg".into()) }],
        outputs: vec![Port { name: "out".into(), ty: Type::Int32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["out".into()],
            rhs: Expr::Field {
                base: Box::new(Expr::var("msg")),
                field: "speed".into(), // not declared
            },
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(node, vec![msg_type]));
    assert!(codes(&report).contains(&"E0120"), "got {:?}", codes(&report));
}

#[test]
fn enum_variant_is_resolved_as_its_enum_type() {
    // type Color = enum { Red, Green, Blue };
    // node Pick() returns (c: Color)
    //   c = Red;
    let color = enum_type("Color", &["Red", "Green", "Blue"]);
    let node = NodeDef {
        name: "Pick".into(),
        kind: NodeKind::Function,
        inputs: vec![],
        outputs: vec![Port { name: "c".into(), ty: Type::Named("Color".into()) }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["c".into()],
            rhs: Expr::var("Red"),
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(node, vec![color]));
    assert!(!report.has_errors(), "got {:?}", codes(&report));
}

#[test]
fn type_alias_resolves_when_comparing_types() {
    // type Altitude = int32;
    // node Pass(x: int32) returns (y: Altitude)
    //   y = x;
    let alt = alias_type("Altitude", Type::Int32);
    let node = NodeDef {
        name: "Pass".into(),
        kind: NodeKind::Function,
        inputs: vec![Port { name: "x".into(), ty: Type::Int32 }],
        outputs: vec![Port { name: "y".into(), ty: Type::Named("Altitude".into()) }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::var("x"),
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(node, vec![alt]));
    assert!(!report.has_errors(), "got {:?}", codes(&report));
}

#[test]
fn integer_literal_adopts_hint_when_in_range() {
    // node K() returns (x: uint8)
    //   x = 5;
    let node = NodeDef {
        name: "K".into(),
        kind: NodeKind::Function,
        inputs: vec![],
        outputs: vec![Port { name: "x".into(), ty: Type::Uint8 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["x".into()],
            rhs: Expr::int_lit(5),
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(node, vec![]));
    assert!(!report.has_errors(), "got {:?}", codes(&report));
}

#[test]
fn integer_literal_out_of_range_for_target_errors() {
    // node K() returns (x: uint8)
    //   x = 500;   -- 500 does not fit in uint8
    let node = NodeDef {
        name: "K".into(),
        kind: NodeKind::Function,
        inputs: vec![],
        outputs: vec![Port { name: "x".into(), ty: Type::Uint8 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["x".into()],
            rhs: Expr::int_lit(500),
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(node, vec![]));
    assert!(codes(&report).contains(&"E0040"), "got {:?}", codes(&report));
}

#[test]
fn arithmetic_with_typed_var_and_literal_keeps_var_type() {
    // node K(a: int16) returns (y: int16)
    //   y = a + 1;
    let node = NodeDef {
        name: "K".into(),
        kind: NodeKind::Function,
        inputs: vec![Port { name: "a".into(), ty: Type::Int16 }],
        outputs: vec![Port { name: "y".into(), ty: Type::Int16 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::bin(ol_ir::BinOp::Add, Expr::var("a"), Expr::int_lit(1)),
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(node, vec![]));
    assert!(!report.has_errors(), "got {:?}", codes(&report));
}

#[test]
fn arithmetic_with_mismatched_widths_still_errors() {
    // node K(a: int16, b: int32) returns (y: int32)
    //   y = a + b;   -- no implicit widening
    let node = NodeDef {
        name: "K".into(),
        kind: NodeKind::Function,
        inputs: vec![
            Port { name: "a".into(), ty: Type::Int16 },
            Port { name: "b".into(), ty: Type::Int32 },
        ],
        outputs: vec![Port { name: "y".into(), ty: Type::Int32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::bin(ol_ir::BinOp::Add, Expr::var("a"), Expr::var("b")),
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(node, vec![]));
    assert!(codes(&report).contains(&"E0086"), "got {:?}", codes(&report));
}

#[test]
fn array_index_must_be_integer() {
    // node Pick(xs: int32[4]) returns (y: int32)
    //   y = xs[true];   -- index is bool, not integer
    let node = NodeDef {
        name: "Pick".into(),
        kind: NodeKind::Function,
        inputs: vec![Port {
            name: "xs".into(),
            ty: Type::Array { elem: Box::new(Type::Int32), len: 4 },
        }],
        outputs: vec![Port { name: "y".into(), ty: Type::Int32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::Index {
                base: Box::new(Expr::var("xs")),
                index: Box::new(Expr::bool_lit(true)),
            },
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(node, vec![]));
    assert!(codes(&report).contains(&"E0111"), "got {:?}", codes(&report));
}

#[test]
fn array_index_with_int_literal_resolves_to_element_type() {
    // node Pick(xs: int32[4]) returns (y: int32)
    //   y = xs[2];
    let node = NodeDef {
        name: "Pick".into(),
        kind: NodeKind::Function,
        inputs: vec![Port {
            name: "xs".into(),
            ty: Type::Array { elem: Box::new(Type::Int32), len: 4 },
        }],
        outputs: vec![Port { name: "y".into(), ty: Type::Int32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::Index {
                base: Box::new(Expr::var("xs")),
                index: Box::new(Expr::int_lit(2)),
            },
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(node, vec![]));
    assert!(!report.has_errors(), "got {:?}", codes(&report));
}

#[test]
fn negative_literal_fits_signed_but_not_unsigned() {
    // node K() returns (s: int8, u: uint8)
    //   s = -10;        -- fits in int8
    //   u = -10;        -- does not fit in uint8
    let node = NodeDef {
        name: "K".into(),
        kind: NodeKind::Function,
        inputs: vec![],
        outputs: vec![
            Port { name: "s".into(), ty: Type::Int8 },
            Port { name: "u".into(), ty: Type::Uint8 },
        ],
        locals: vec![],
        equations: vec![
            Equation {
                lhs: vec!["s".into()],
                rhs: Expr::neg(Expr::int_lit(10)),
            },
            Equation {
                lhs: vec!["u".into()],
                rhs: Expr::neg(Expr::int_lit(10)),
            },
        ],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(node, vec![]));
    // The signed assignment is fine; the unsigned assignment must error.
    let cs = codes(&report);
    assert!(
        cs.iter().filter(|c| **c == "E0040").count() == 1,
        "expected exactly one E0040, got {cs:?}"
    );
}
