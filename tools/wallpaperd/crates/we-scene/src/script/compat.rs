//! Parse expressions before lowering WE vector arithmetic; never rewrite strings/comments.
use anyhow::{Result, ensure};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    AssignmentExpression, AssignmentTarget, BinaryExpression, Declaration, Expression, Statement,
    UnaryExpression,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};

pub(super) fn lower(source: &str) -> Result<String> {
    ensure!(
        source.len() <= 1024 * 1024,
        "SceneScript source exceeds 1 MiB"
    );
    let source = metadata(source);
    let source = source.as_str();
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
    ensure!(
        parsed.diagnostics.is_empty(),
        "SceneScript syntax: {:?}",
        parsed.diagnostics.first()
    );
    let mut rewrite = Rewrite {
        source,
        edits: Vec::new(),
    };
    rewrite.visit_program(&parsed.program);
    Ok(rewrite.render(Span::new(0, source.len() as u32)))
}
// Editor-added workshop provenance is metadata, not executable declarations.
// A duplicated, identical ID can occur when an effect is pasted twice. Preserve
// the first export and line offsets; conflicting IDs and other redeclarations
// retain the standard JS syntax error.
fn metadata(source: &str) -> String {
    if !source.contains("__workshopId") {
        return source.into();
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
    let mut seen = None;
    let mut result = source.to_owned();
    for statement in &parsed.program.body {
        if let Statement::ExportDeclaration(export) = statement
            && let Declaration::VariableDeclaration(declaration) = &export.declaration
            && declaration.declarations.len() == 1
            && let Some(identifier) = declaration.declarations[0].id.get_binding_identifier()
            && identifier.name == "__workshopId"
            && let Some(Expression::StringLiteral(value)) = &declaration.declarations[0].init
            && !value.value.is_empty()
            && value.value.len() <= 32
            && value.value.chars().all(|c| c.is_ascii_digit())
        {
            if seen == Some(value.value.as_str()) {
                let span = export.span;
                let replacement = source[span.start as usize..span.end as usize]
                    .bytes()
                    .map(|b| if b == b'\n' { '\n' } else { ' ' })
                    .collect::<String>();
                result.replace_range(span.start as usize..span.end as usize, &replacement);
            } else if seen.is_none() {
                seen = Some(value.value.as_str());
            }
        }
    }
    result
}
struct Rewrite<'s> {
    source: &'s str,
    edits: Vec<(Span, String)>,
}
impl Rewrite<'_> {
    fn render(&self, span: Span) -> String {
        let mut result = String::new();
        let mut cursor = span.start;
        for (edit, replacement) in &self.edits {
            if edit.start >= span.start && edit.end <= span.end {
                result.push_str(&self.source[cursor as usize..edit.start as usize]);
                result.push_str(replacement);
                cursor = edit.end;
            }
        }
        result.push_str(&self.source[cursor as usize..span.end as usize]);
        result
    }
    fn replace(&mut self, span: Span, value: String) {
        self.edits
            .retain(|(edit, _)| !(edit.start >= span.start && edit.end <= span.end));
        let index = self
            .edits
            .partition_point(|(edit, _)| edit.start < span.start);
        self.edits.insert(index, (span, value));
    }
}
impl<'a> Visit<'a> for Rewrite<'_> {
    fn visit_binary_expression(&mut self, expression: &BinaryExpression<'a>) {
        walk::walk_binary_expression(self, expression);
        let op = expression.operator.as_str();
        if ["+", "-", "*", "/"].contains(&op) {
            self.replace(
                expression.span,
                format!(
                    "__weBinary({op:?},({}),({}))",
                    self.render(expression.left.span()),
                    self.render(expression.right.span())
                ),
            );
        }
    }
    fn visit_unary_expression(&mut self, expression: &UnaryExpression<'a>) {
        walk::walk_unary_expression(self, expression);
        let op = expression.operator.as_str();
        if ["+", "-"].contains(&op) {
            self.replace(
                expression.span,
                format!(
                    "__weUnary({op:?},({}))",
                    self.render(expression.argument.span())
                ),
            );
        }
    }
    fn visit_assignment_expression(&mut self, expression: &AssignmentExpression<'a>) {
        walk::walk_assignment_expression(self, expression);
        let Some(op) = expression.operator.as_str().strip_suffix('=') else {
            return;
        };
        if !["+", "-", "*", "/"].contains(&op) {
            return;
        }
        let right = self.render(expression.right.span());
        let replacement = match &expression.left {
            AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                format!("({0}=__weBinary({op:?},{0},({right})))", identifier.name)
            }
            AssignmentTarget::StaticMemberExpression(member) if !member.object.is_super() => {
                format!(
                    "__weRef(({}),{:?}).assign({op:?},({right}))",
                    self.render(member.object.span()),
                    member.property.name.as_str()
                )
            }
            AssignmentTarget::ComputedMemberExpression(member) if !member.object.is_super() => {
                format!(
                    "__weRef(({}),({})).assign({op:?},({right}))",
                    self.render(member.object.span()),
                    self.render(member.expression.span())
                )
            }
            _ => return,
        };
        self.replace(expression.span, replacement);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn workshop_provenance_only_deduplicates_identical_literal_ids() {
        let source = "// __workshopId\nexport let __workshopId='123';\nexport let\u{a0}__workshopId='123';\nexport function update(v){return v;}";
        let cleaned = super::metadata(source);
        assert_eq!(cleaned.len(), source.len());
        assert_eq!(cleaned.lines().count(), source.lines().count());
        assert_eq!(cleaned.matches("export let").count(), 1);
        assert!(super::lower(source).is_ok());
        for source in [
            "export let __workshopId='123'; export let __workshopId='456';",
            "export let __workshopId='123'; export let __workshopId=String(123);",
            "export let other=1; export let other=1;",
        ] {
            assert_eq!(super::metadata(source), source);
        }
    }
}
