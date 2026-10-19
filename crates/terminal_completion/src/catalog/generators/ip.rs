use crate::metadata::{
    CommandBuilder, CommandSignatureGenerators, Generator, GeneratorResults,
    GeneratorResultsCollector, Suggestion,
};

/// Parses `ip -o link show` output into interface-name suggestions.
///
/// Each line has the shape `<index>: <name>[@<parent>]: <FLAGS> mtu ... state <STATE> ...`.
/// The name is what `dev`/`iif`/`oif`-style arguments take; a `@parent` suffix (used by
/// tunnels and VLANs to show their underlying device) is stripped since it isn't part of
/// the interface name itself. The device's operational state is folded into the
/// suggestion's description so a disconnected or down interface is still recognisable.
fn parse_interfaces(output: &str) -> GeneratorResults {
    output
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.split_once(": ")?;
            let (name, details) = rest.split_once(": ")?;
            let name = name.split('@').next()?.trim();
            if name.is_empty() {
                return None;
            }
            Some(Suggestion::with_description(
                name,
                interface_state(details).unwrap_or_else(|| "Network interface".to_string()),
            ))
        })
        .collect_ordered_results()
}

/// Describes an interface from its operational `state` field, e.g. `UP` or `DOWN`.
fn interface_state(details: &str) -> Option<String> {
    let (_, after_state) = details.split_once("state ")?;
    let state = after_state.split_whitespace().next()?;
    Some(format!("Interface ({state})"))
}

/// Parses `ip tunnel show` output into tunnel-name suggestions.
///
/// Each configured tunnel gets its own line, shaped `<name>: <mode>/ip remote <addr> local
/// <addr> ttl <ttl> [nopmtudisc] ...` (e.g. `gre1: gre/ip remote 10.0.0.2 local 10.0.0.1 ttl
/// 255`). The encapsulation mode (the part before `/ip`) is folded into the suggestion's
/// description so e.g. a GRE tunnel is distinguishable from a SIT one.
///
/// Only positions that reference an *existing* tunnel should use this -- `tunnel change`,
/// `tunnel delete`, and `tunnel show`'s own optional NAME. `tunnel add`'s NAME names a tunnel
/// being created, so nothing should be suggested there.
fn parse_tunnels(output: &str) -> GeneratorResults {
    output
        .lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some(Suggestion::with_description(
                name,
                tunnel_mode(rest).unwrap_or_else(|| "IP tunnel".to_string()),
            ))
        })
        .collect_ordered_results()
}

/// Describes a tunnel from its encapsulation mode, e.g. `gre/ip` becomes `gre tunnel`.
fn tunnel_mode(rest: &str) -> Option<String> {
    let mode = rest.trim().split('/').next()?.trim();
    if mode.is_empty() {
        return None;
    }
    Some(format!("{mode} tunnel"))
}

/// Parses `ps -eo pid,comm` output (dropping the header row) into PID suggestions
/// described by the process's command name.
///
/// Used for positions that accept the PID of an arbitrary running process, such as
/// `ip netns attach NAME PID` and `ip netns identify [PID]` — these accept any
/// process on the system, not one already associated with a namespace.
fn parse_processes(output: &str) -> GeneratorResults {
    output
        .lines()
        .skip(1) // drop the "PID COMMAND" header
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?;
            if pid.is_empty() {
                return None;
            }
            let comm = parts.collect::<Vec<_>>().join(" ");
            Some(Suggestion::with_description(
                pid,
                if comm.is_empty() {
                    "Process".to_string()
                } else {
                    comm
                },
            ))
        })
        .collect_unordered_results()
}

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("ip")
        .add_generator(
            "netns",
            Generator::script(
                CommandBuilder::single_command("ip netns list 2>/dev/null"),
                |output| {
                    output
                        .lines()
                        .filter_map(|line| {
                            // Output format: "name" or "name (id: N)"
                            let name = line.split_whitespace().next()?;
                            if name.is_empty() {
                                return None;
                            }
                            Some(Suggestion::with_description(name, "Network namespace"))
                        })
                        .collect_unordered_results()
                },
            ),
        )
        .add_generator(
            "network_interfaces",
            Generator::script(
                // `-o` keeps each interface on its own line so it can be parsed reliably.
                CommandBuilder::single_command_and_ignore_stderr("ip -o link show"),
                parse_interfaces,
            ),
        )
        .add_generator(
            "tunnel_interfaces",
            Generator::script(
                CommandBuilder::single_command_and_ignore_stderr("ip tunnel show"),
                parse_tunnels,
            ),
        )
        .add_generator(
            "processes",
            Generator::script(
                CommandBuilder::single_command("ps -eo pid,comm"),
                parse_processes,
            ),
        )
}
