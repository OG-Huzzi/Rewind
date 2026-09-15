//! Phase 2: the minimum interactive surface.
//!
//! This surface is **presentation only**. It renders values produced by the same
//! domain API the CLI uses (`plan::plan_rollback`, `DependencyGraph`), and it is
//! structurally incapable of decision-making or mutation:
//!
//! * [`parse`] and [`render`] are pure: `render` returns the text it would show,
//!   so the whole surface is testable without a terminal.
//! * there is no command that executes anything. The mutating verbs are
//!   recognised only to be refused with an explanation.
//!
//! No new dependency, no alternate screen, no cursor control.

use std::io::BufRead;

use crate::depgraph::DependencyGraph;
use crate::error::Result;
use crate::model::OperationRecord;
use crate::plan::{self, PlanDecision, RollbackTarget};
use crate::workspace::Workspace;

const HELP: &str = "\
commands:
  history [n]          recorded operations, oldest first (default 20)
  graph [operation]    the dependency graph, or the edges incident to one operation
  plan undo <id>...    what a rollback would do; never executes
  plan redo <id>       the same for one redo target
  help                 this text
  quit                 leave
";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiAction {
    Help,
    Quit,
    History {
        limit: usize,
    },
    Graph {
        operation: Option<i64>,
    },
    Plan {
        targets: Vec<RollbackTarget>,
    },
    /// A command that this surface will not perform.
    Refused {
        verb: String,
        detail: String,
    },
    Invalid {
        input: String,
        detail: String,
    },
}

/// Parse one line. Pure.
pub fn parse(line: &str) -> UiAction {
    let mut parts = line.split_whitespace();
    let Some(head) = parts.next() else {
        return UiAction::Help;
    };
    match head {
        "help" | "?" => UiAction::Help,
        "quit" | "exit" | "q" => UiAction::Quit,
        "history" => UiAction::History {
            limit: parts
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(20),
        },
        "graph" => UiAction::Graph {
            operation: parts.next().and_then(|value| value.parse().ok()),
        },
        "plan" => {
            let direction = parts.next().unwrap_or("");
            let ids: Vec<i64> = parts.filter_map(|value| value.parse().ok()).collect();
            match (direction, ids.len()) {
                ("undo", count) if count > 0 => UiAction::Plan {
                    targets: ids.into_iter().map(RollbackTarget::undo).collect(),
                },
                ("redo", 1) => UiAction::Plan {
                    targets: vec![RollbackTarget::redo(ids[0])],
                },
                ("undo", _) => UiAction::Invalid {
                    input: line.trim().to_owned(),
                    detail: "usage: plan undo <id> [<id>...]".to_owned(),
                },
                ("redo", _) => UiAction::Invalid {
                    input: line.trim().to_owned(),
                    detail: "usage: plan redo <id>".to_owned(),
                },
                _ => UiAction::Invalid {
                    input: line.trim().to_owned(),
                    detail: "usage: plan undo <id>... | plan redo <id>".to_owned(),
                },
            }
        }
        "apply" | "undo" | "redo" | "restore" | "snapshot" | "reconcile" | "recover" => {
            UiAction::Refused {
                verb: head.to_owned(),
                detail: "this surface is read-only. It renders plans and never executes or \
                         mutates them; use `rewind apply rollback` explicitly."
                    .to_owned(),
            }
        }
        _ => UiAction::Invalid {
            input: line.trim().to_owned(),
            detail: "unknown command; type `help`".to_owned(),
        },
    }
}

/// Render the answer to one action. Read-only: the only workspace access is
/// through the same read-only API the CLI uses.
pub fn render(workspace: &Workspace, action: &UiAction) -> Result<String> {
    match action {
        UiAction::Help | UiAction::Quit => Ok(HELP.to_owned()),
        UiAction::History { limit } => {
            let mut operations = workspace.storage.catalog.list_operations(workspace.id)?;
            operations.sort_by_key(|operation| (operation.created_at, operation.id));
            if operations.len() > *limit {
                operations.drain(..operations.len() - limit);
            }
            Ok(render_history(&operations))
        }
        UiAction::Graph { operation } => {
            let graph = DependencyGraph::build(workspace)?;
            Ok(render_graph(&graph, *operation))
        }
        UiAction::Plan { targets } => {
            let plan = plan::plan_rollback(workspace, targets)?;
            Ok(render_plan(&plan))
        }
        UiAction::Refused { verb, detail } => Ok(format!("refused: {verb}\n{detail}\n")),
        UiAction::Invalid { input, detail } => Ok(format!("invalid: {input}\n{detail}\n")),
    }
}

fn render_history(operations: &[OperationRecord]) -> String {
    let mut text = String::new();
    for operation in operations {
        text.push_str(&format!(
            "{:>5}  {:<19} {:<10} {:<17} {}\n",
            operation.id,
            operation.kind.as_str(),
            operation.status.as_str(),
            operation.reversibility.as_str(),
            operation
                .command
                .clone()
                .unwrap_or_else(|| "<no command>".to_owned())
        ));
    }
    text
}

fn render_graph(graph: &DependencyGraph, operation: Option<i64>) -> String {
    let filter = operation.map(crate::depgraph::NodeId::operation);
    let mut text = format!(
        "{} nodes, {} edges, {} cycles, {} incomplete\n",
        graph.nodes.len(),
        graph.edges.len(),
        graph.cycles.len(),
        graph.incomplete.len()
    );
    for edge in &graph.edges {
        if let Some(node) = &filter {
            if &edge.from != node && &edge.to != node {
                continue;
            }
        }
        text.push_str(&format!(
            "  {} {} {}  [{}]\n",
            edge.from,
            match edge.confidence {
                crate::depgraph::EdgeConfidence::Known => "->",
                crate::depgraph::EdgeConfidence::Advisory => "~>",
            },
            edge.to,
            edge.evidence.as_str()
        ));
    }
    if filter.is_none() {
        for record in &graph.incomplete {
            text.push_str(&format!(
                "  ? operation {}  {}  {}\n",
                record.operation_id,
                record.evidence.as_str(),
                record.detail
            ));
        }
    }
    text
}

fn render_plan(plan: &plan::RollbackPlan) -> String {
    let mut text = format!(
        "decision {}\n",
        match plan.decision {
            PlanDecision::Executable => "EXECUTABLE",
            PlanDecision::Refused => "REFUSED",
        }
    );
    for (position, step) in plan.order.iter().enumerate() {
        let reason = match step.reason {
            plan::IncludedReason::Selected => "selected".to_owned(),
            plan::IncludedReason::RequiredBy { operation_id } => {
                format!("required by {operation_id}")
            }
        };
        text.push_str(&format!(
            "  {}. undo operation {} ({reason})\n",
            position + 1,
            step.operation_id
        ));
    }
    for reason in &plan.block_reasons {
        text.push_str(&format!("  blocked: {reason:?}\n"));
    }
    for conflict in &plan.conflicts {
        text.push_str(&format!("  conflict: {conflict:?}\n"));
    }
    if !plan.advisory_context.is_empty() {
        text.push_str(&format!(
            "  {} advisory edges shown as context and not acted on\n",
            plan.advisory_context.len()
        ));
    }
    text.push_str("nothing was executed\n");
    text
}

/// Read lines from stdin and answer them until `quit` or end of input. This is
/// the only part that needs a terminal, and it decides nothing.
pub fn run(workspace: &Workspace) -> Result<i32> {
    println!("rewind inspect (read-only). `help` for commands, `quit` to leave.");
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        let action = parse(&line);
        if action == UiAction::Quit {
            break;
        }
        print!("{}", render(workspace, &action)?);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_undo_accepts_many_ids() {
        assert_eq!(
            parse("plan undo 3 4"),
            UiAction::Plan {
                targets: vec![RollbackTarget::undo(3), RollbackTarget::undo(4)]
            }
        );
    }

    #[test]
    fn plan_redo_accepts_exactly_one_id() {
        assert_eq!(
            parse("plan redo 7"),
            UiAction::Plan {
                targets: vec![RollbackTarget::redo(7)]
            }
        );
        assert!(matches!(parse("plan redo 7 8"), UiAction::Invalid { .. }));
    }

    #[test]
    fn mutating_verbs_are_refused_not_ignored() {
        for verb in ["apply", "undo", "redo", "restore", "snapshot"] {
            match parse(verb) {
                UiAction::Refused { verb: refused, .. } => assert_eq!(refused, verb),
                other => panic!("{verb} must be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_unknown_command_is_reported_as_invalid() {
        assert!(matches!(parse("frobnicate"), UiAction::Invalid { .. }));
    }

    #[test]
    fn an_empty_line_shows_help() {
        assert_eq!(parse("   "), UiAction::Help);
    }
}
