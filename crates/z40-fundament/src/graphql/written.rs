//! The limits of the written document (`docs/protocol.md` §4.2), before anything is expanded: fragment
//! spreads, arguments per field and directive and in total, directives per location, variable
//! definitions per operation and the length of names.
//!
//! The engine compares the arguments of fields with the same response name, quadratic in the number of
//! arguments per field, and walks directives per location; these counts keep that work small. It also
//! repeats names in its error messages, once per error, so no name may be long. One walk with an explicit
//! stack (no recursion, however deep the document), stopping at the first broken limit.

use super::{SubgraphLimits, Violation};
use async_graphql::parser::types::{
    BaseType, Directive, ExecutableDocument, Selection, SelectionSet, Type,
};
use async_graphql::{Name, Positioned};
use async_graphql_value::Value;

/// Checks the written counts of `doc` against `limits`.
pub(super) fn check_written(
    doc: &ExecutableDocument,
    limits: &SubgraphLimits,
) -> Result<(), Violation> {
    let mut w = Written {
        limits,
        spreads: 0,
        arguments: 0,
    };
    let mut stack: Vec<&SelectionSet> = Vec::new();
    for (name, op) in doc.operations.iter() {
        let op = &op.node;
        if op.variable_definitions.len() > limits.max_variables() {
            return Err(Violation::Variables);
        }
        if let Some(name) = name {
            w.name(name)?;
        }
        w.directives(&op.directives)?;
        for var in &op.variable_definitions {
            w.name(&var.node.name.node)?;
            w.var_type(&var.node.var_type.node)?;
            w.directives(&var.node.directives)?;
        }
        stack.push(&op.selection_set.node);
    }
    for (name, def) in doc.fragments.iter() {
        w.name(name)?;
        w.name(&def.node.type_condition.node.on.node)?;
        w.directives(&def.node.directives)?;
        stack.push(&def.node.selection_set.node);
    }
    while let Some(set) = stack.pop() {
        for item in &set.items {
            match &item.node {
                Selection::Field(f) => {
                    let f = &f.node;
                    w.name(&f.name.node)?;
                    if let Some(alias) = &f.alias {
                        w.name(&alias.node)?;
                    }
                    w.arguments(&f.arguments)?;
                    w.directives(&f.directives)?;
                    stack.push(&f.selection_set.node);
                }
                Selection::InlineFragment(f) => {
                    if let Some(on) = &f.node.type_condition {
                        w.name(&on.node.on.node)?;
                    }
                    w.directives(&f.node.directives)?;
                    stack.push(&f.node.selection_set.node);
                }
                Selection::FragmentSpread(s) => {
                    w.spreads += 1;
                    if w.spreads > limits.max_spreads() {
                        return Err(Violation::Spreads);
                    }
                    w.name(&s.node.fragment_name.node)?;
                    w.directives(&s.node.directives)?;
                }
            }
        }
    }
    Ok(())
}

/// The counts of the walk so far.
struct Written<'a> {
    limits: &'a SubgraphLimits,
    spreads: usize,
    arguments: usize,
}

impl Written<'_> {
    /// Whether `name` is short enough.
    fn name(&self, name: &str) -> Result<(), Violation> {
        if name.len() > self.limits.max_name_length() {
            return Err(Violation::NameLength);
        }
        Ok(())
    }

    /// Checks the named type of a variable definition (`[[T!]]` names `T`).
    fn var_type(&self, ty: &Type) -> Result<(), Violation> {
        let mut ty = ty;
        loop {
            match &ty.base {
                BaseType::Named(name) => return self.name(name),
                BaseType::List(inner) => ty = inner,
            }
        }
    }

    /// Counts the arguments of one field or directive and checks their names and the names of the
    /// variables their values use.
    fn arguments(
        &mut self,
        arguments: &[(Positioned<Name>, Positioned<Value>)],
    ) -> Result<(), Violation> {
        let n = arguments.len();
        if n > self.limits.max_arguments() {
            return Err(Violation::Arguments);
        }
        self.arguments = self.arguments.saturating_add(n);
        if self.arguments > self.limits.max_total_arguments() {
            return Err(Violation::TotalArguments);
        }
        for (name, value) in arguments {
            self.name(&name.node)?;
            self.variables_in(&value.node)?;
        }
        Ok(())
    }

    /// Checks the names of the variables used in `value` (explicit stack, however deep the value).
    fn variables_in(&self, value: &Value) -> Result<(), Violation> {
        let mut stack = vec![value];
        while let Some(v) = stack.pop() {
            match v {
                Value::Variable(name) => self.name(name)?,
                Value::List(l) => stack.extend(l),
                Value::Object(o) => stack.extend(o.values()),
                _ => {}
            }
        }
        Ok(())
    }

    /// Counts the directives of one location and their arguments, and checks their names.
    fn directives(&mut self, directives: &[Positioned<Directive>]) -> Result<(), Violation> {
        if directives.len() > self.limits.max_directives() {
            return Err(Violation::Directives);
        }
        for d in directives {
            self.name(&d.node.name.node)?;
            self.arguments(&d.node.arguments)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "written_tests.rs"]
mod tests;
