# Surfer Simulator Protocol

This protocol is used for real-time communication between Surfer and external simulators. It was created for Surfer to communicate with NVC using a VHPI plugin. Much of the implementation is based on how Surfer and wellen handle VHDL data.

This document explains the protocol structure so implementations can be made for other simulators. It is not fully comprehensive, and focuses on what is not already covered by schema/comments. Refer to the VHPI plugin for a reference implementation.

Surfer connects to a simulator/plugin over TCP (default port 6967). The protocol is encoded using Protocol Buffers (protobuf) with length-delimited frames (tokio_util::codec::LengthDelimitedCodec).

## Timeline

Startup and normal flow are:

1. Simulator send `SimulatorInfo` upon connection, including `protocol_version`, `name`, and `version`.
2. Surfer sends `RequestHierarchy`.
3. Simulator replies with `Hierarchy`.
4. Surfer sends `TrackVariableChanges` and `UntrackVariableChanges` as the user selects/deselects variables.
5. Surfer sends `RunSimulation` (optionally with a duration) and `PauseSimulation`.
6. Simulator sends `ValueChanges` and optionally `CurrentSimulationTime` updates while running.

## Hierarchy

The hierarchy is a vector of `HierarchyElement` values. These can be `PushScope`, `PopScope`, and `AddVar`. Every `PushScope` must be paired with a matching `PopScope`.

`PushScope` and `PopScope` should be used for arrays and records/structs. For arrays, it is recommended that the name is the array index. Use `AddVar` for leaf variables. For names, use simulator-native naming consistently so the frontend can map variables correctly.

`Hierarchy` includes a decimal timestamp exponent so Surfer knows how simulator timestamps map to time units used in `RunSimulation` and `TimeStep` timestamps.

## Variable id:s

The simulator (plugin) is responsible for creating a unique id for each variable in the hierarchy.

## Variable Tracking

`TrackVariableChanges` can be sent for the same variable multiple times. Only the first call should have an effect. After `UntrackVariableChanges`, variable changes for that variable should no longer be sent.

## Signal Changes

Signal value changes should use an encoding consistent with each variable's declared `SignalEncoding` in `Hierarchy`/`AddVar`.
