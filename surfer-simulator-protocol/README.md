# Surfer Simulator Protocol

This is a protocol used for communicating with external simulators in real-time. It was created for Surfer to communicate in with NVC using a VHPI-plugin. Much of the implementation is based on how Surfer and wellen handle VHD-files.

This document aims to document and explain some how the protocol is structured so that implementations can be made with other simulators. This document is not fully comprehensive, instead it aims to focus on that which is not covered in the code comments for the protocol. Refer to the VHPI-plugin for a reference implementation.

Surfer listens for TCP-communication over the port 6769. The protocol is encoded using bincode v1.3.3 using `tokio_util::codec::LengthDelimitedCodec`.

## Timeline

First Surfer will request the Hierarchy from simulator. Then the user will request what variables to record values for. This is sent to the plugin using the SetCallback and RemoveCallback messages. After that the RunSimulation message is called. After that the simulator will continuously send SendChangedValues packets for the requested time.

## Hierarchy

The hierarchy a vector of HierarchyElem:s. These can be of the kinds PushScope, PopScope, and AddVar. A PushScope should always be paired with an equivalent PopScope.

PushScope and PopScope should not be used for array types, instead these should be in the variable name in AddVar. These names should be formatted in the same way as a VHDL-file, so the name for a variable could be `MY_ARRAY[2][7:2]`. This is parsed into appropriate scopes by wellen. AddVar and maps on to one to a single variable.

Along with the hierarchy the message sends a unit so that Surfer knows how big one unit of time used by the simulator is in the RunSimulation message and TimeStep:s timestamp.

## Variable id:s

The simulator is responsible for creating a unique id for each variable in the hierarchy.

## Callbacks

SetCallback can be sent for the same variable multiple times. Only the first call should do something. Whenever RemoveCallback is sent, variable changes for that variable should no longer be sent, no matter how many times SetCallback has been sent before.

## Signal Changes

The value of a variable change is sent in the same way as a value is encoded in a VCD-file, these should use the same type as the ones sent in variable_encoding in the Hierarchy.
