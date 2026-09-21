# ADR-0003 — Hand-rolled UPnP rather than a framework

**Status:** accepted · **Date:** 2026-09-20

## Context

Rincon speaks UPnP to a Sonos player: SSDP for discovery, SOAP for control. Crates exist that
implement UPnP generically.

Our surface is five actions: `SetAVTransportURI`, `Play`, `Stop`, `GetVolume`, `SetVolume`,
plus `GetZoneGroupState`.

## Options

**A UPnP framework crate.** Handles discovery, service description parsing, action invocation,
and eventing. Roughly 200 lines of integration against several thousand lines of dependency.

**Hand-rolled.** About 400 lines across `soap.rs`, `didl.rs`, and `topology.rs`.

## Decision

Hand-rolled.

## Why

Three reasons, in order of weight.

**The part that matters is the part a framework hides.** Every UPnP action interpolates values
into an XML document sent to a device on the user's network. Unescaped interpolation is a
textbook injection bug. We need to *know* that every value is escaped, and to prove it with
property tests over generated hostile input. A framework's escaping might well be correct — but
verifying that is more work than writing 400 lines we can test directly.

**Sonos is not generic UPnP.** The DIDL-Lite metadata needs a specific `cdudn` token or the
speaker accepts the command and plays nothing. Group coordinators need resolving before any
transport command, or grouped households silently fail. Topology arrives as XML escaped inside
XML. A generic framework helps with none of this and obscures all of it.

**Security budget.** Every dependency in a program that parses untrusted network input is
surface we audit and track advisories for. A framework here is several thousand lines of it to
save 400 we understand completely.

## Consequences

**Good.** We control escaping and can prove it — the property test asserts that no generated
value changes the envelope's structure. Error handling maps UPnP codes onto actionable
variants rather than a generic fault type. The XML guard (no DOCTYPE, ever) applies uniformly
because we call it.

**Bad.** Supporting a second kind of UPnP device would mean writing more of the protocol. We
carry the maintenance of code a crate would maintain for us.

**Accepted.** SPEC-004 §3 rules out queues, favourites, alarms, and eventing. The surface is
not going to grow much, and if it does, the escaping is already the part we would have had to
verify anyway.

## Revisit if

Rincon ever targets non-Sonos UPnP renderers, at which point the generic parts become worth
importing rather than writing.
