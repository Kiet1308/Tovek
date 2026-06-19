use crate::{Block, RcLocal};
use itertools::Itertools;
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt::{Display, Formatter},
};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub enum Type {
    Any,
    Nil,
    Boolean,
    Number,
    String,
    Table {
        indexer: Box<(Type, Type)>,
        fields: BTreeMap<String, Type>,
    },
    Function(Vec<Type>, Vec<Type>),
    Optional(Box<Type>),
    Union(BTreeSet<Type>),
    Intersection(BTreeSet<Type>),
    VarArg,
    Vector,
}

impl Type {
    pub fn is_subtype_of(&self, t: &Self) -> bool {
        match t {
            Self::Any => true,
            Self::Table {
                box indexer,
                fields,
            } => {
                let t_fields = fields;
                let (indexer_type, element_type) = indexer;

                match self {
                    Self::Table {
                        box indexer,
                        fields,
                    } if indexer.0.is_subtype_of(indexer_type)
                        && indexer.1.is_subtype_of(element_type) =>
                    {
                        t_fields
                            .keys()
                            .all(|k| fields[k].is_subtype_of(&t_fields[k]))
                    }
                    _ => false,
                }
            }
            Self::Union(union) => match self {
                Self::Union(u) => union.iter().all(|t| u.contains(t)),
                _ => union.contains(self),
            },
            _ => false,
        }
    }

    pub fn precedence(&self) -> usize {
        match self {
            Self::Any => 0,
            Self::Nil => 0,
            Self::Boolean => 0,
            Self::Number => 0,
            Self::String => 0,
            Self::Table { .. } => 1,
            Self::Function(_, _) => 1,
            Self::Optional(_) => 0,
            Self::Union(_) => 2,
            Self::Intersection(_) => 2,
            Self::VarArg => 0,
            Self::Vector => 0,
        }
    }
}

impl Display for Type {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Type::Any => Cow::Borrowed("any"),
                Type::Nil => Cow::Borrowed("nil"),
                Type::Boolean => Cow::Borrowed("boolean"),
                Type::Number => Cow::Borrowed("number"),
                Type::String => Cow::Borrowed("string"),
                Type::Table { indexer, fields } => {
                    let (indexer_type, element_type) = indexer.as_ref();

                    Cow::Owned(format!(
                        "{{{}{}{}}}",
                        if indexer_type == &Type::Number && fields.is_empty() {
                            element_type.to_string()
                        } else {
                            format!("[{}]: {}", indexer_type, element_type)
                        },
                        (!fields.is_empty()).then_some(", ").unwrap_or_default(),
                        fields
                            .iter()
                            .map(|(field, r#type)| { format!("{}: {}", field, r#type) })
                            .join(", ")
                    ))
                }
                Type::Function(domain, codomain) => Cow::Owned(format!(
                    "({}) -> {}",
                    domain.iter().join(", "),
                    if (codomain.len() == 1 && self.precedence() >= codomain[0].precedence())
                        || codomain.len() > 1
                    {
                        format!("({})", codomain.iter().join(", "))
                    } else {
                        codomain.iter().join(", ")
                    }
                )),
                Type::Optional(r#type) => Cow::Owned(format!("{}?", r#type)),
                Type::Union(types) => {
                    Cow::Owned(types.iter().join(" | "))
                }
                Type::Intersection(types) => {
                    Cow::Owned(types.iter().join(" & "))
                }
                Type::VarArg => Cow::Borrowed("..."),
                Type::Vector => Cow::Borrowed("vector"),
            }
        )
    }
}

pub struct TypeSystem<'a> {
    // TODO: use hash map?
    annotations: BTreeMap<&'a RcLocal, &'a mut Type>,
}

impl<'a> TypeSystem<'a> {
    pub fn analyze(block: &'a mut Block) {
        let mut system = Self {
            annotations: BTreeMap::new(),
        };

        system.analyze_block(block);
    }

    pub fn analyze_block(&mut self, _block: &'a mut Block) -> Vec<Type> {
        todo!()
        // let mut return_values = Vec::new();

        // for statement in &mut block.0 {
        //     match statement {
        //         /*Statement::Assign(assign) => {
        //             for ((lvalue, annotation), rvalue) in
        //                 assign.left.iter_mut().zip(assign.right.iter_mut())
        //             {
        //                 let r#type = rvalue.infer(self);

        //                 if let LValue::Local(local) = lvalue {
        //                     if let Some(annotation) = self.annotations.get_mut(local) {
        //                         if let Type::Union(types) = annotation {
        //                             types.insert(r#type);
        //                         } else {
        //                             let mut types = BTreeSet::new();

        //                             types.insert(annotation.clone());
        //                             types.insert(r#type);

        //                             **annotation = Type::Union(types);
        //                         }
        //                     } else {
        //                         *annotation = Some(r#type);
        //                         self.annotations.insert(local, annotation.as_mut().unwrap());
        //                     }
        //                 }
        //             }
        //         }*/
        //         Statement::If(r#if) => {
        //             self.analyze_block(&mut r#if.then_block);
        //             self.analyze_block(&mut r#if.else_block);
        //         }
        //         // TODO: repeat and for loops
        //         Statement::While(r#while) => {
        //             self.analyze_block(&mut r#while.block.lock());
        //         }
        //         Statement::Return(r#return) => {
        //             return_values.extend(r#return.values.iter_mut().map(|v| v.infer(self)));
        //         }
        //         // TODO: numericfor, genericfor
        //         _ => {}
        //     }
        // }

        // return_values
    }

    pub fn type_of(&self, local: &RcLocal) -> &Type {
        self.annotations
            .get(local)
            .map(|t| t as &Type)
            .unwrap_or(&Type::Any)
    }
}

pub trait Infer {
    fn infer<'a: 'b, 'b>(&'a mut self, system: &mut TypeSystem<'b>) -> Type;
}
