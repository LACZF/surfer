# Simulation

It is possible to connect to simulators and get the waveform information directly from the simulator, without the need to generate a waveform file. There is also some basic support to compile code and start the simulator from within Surfer. It may be worth noting that one can start a simulator at a different computer, setup an SSH-tunnel between the computers and connect with Surfer. Also, note that it is possible to start the simulation on WSL and connect with the Windows version of Surfer without any additional work.

These features are experimental and we expect that they will improve over time. Please open an issue if you have suggestions of how it can be improved or find an issue.

For the VS Code extension it is possible to start the simulator with the correct plugin and then connect to the simulator.

Note that this is not available in the browser version.

## VPI

VPI (Verilog Procedural Interface) is a standardized C-language application programming interface defined by the IEEE 1800 standard that allows external software to interact with a Verilog/SystemVerilog simulation at runtime.

There is a plugin, `surfer_vpi_plugin`, that can be started with a simulator that supports VPI. This has been tested and shown to work (to some extent) with Icarus Verilog and Xezim. It is included in the downloadable zip-files or can be built locally.

If required, build the plugin:

```bash
cargo build --release --package surfer-vpi-plugin
```

Copy the file from `./target/release`, where the filename depends on the platform:

* Linux: `libsurfer_vpi_plugin.so`
* Macos: `libsurfer_vpi_plugin.dylib`
* Windows: `surfer_vpi_plugin.dll`

For Icarus Verilog, you may want to change the extension to `.vpi`, especially on Windows.

### Icarus Verilog

If Surfer can find Icarus Verilog on your computer, there will be an Icarus Verilog submenu under Simulation.

#### Start Icarus Verilog from within Surfer

You can select Simulation -> Icarus Verilog -> Compile... to compile a number of files into a single vvp. This vvp is called `surfer_default.vvp`.

If you have compiled your files into vvp-files in the current directory or used the Compile... menu option, you can select Simulation -> Icarus Verilog -> Run simulation... Now there will be a possibility to select a design to simulate.

#### Start Icarus Verilog separately and connect

Compile the sources as usual:

```bash
iverilog -o design.vvp file1.v file2.v ...
```

When starting the simulation, supply the plugin with the `-m` switch:

```bash
vvp -m./surfer_vpi_plugin.vpi -i design.vvp
```

You should now see something like (the `-i` flag is to drop the buffering of the output):

```text
Start of simulation entered
No valid port startup argument found, using default 6967
Waiting for RunSimulation message...
```

Now, you can select Simulation -> Connect to running simulator...

To change the port, you can supply a port using a `+` argument, as:

```bash
vvp -m./surfer_vpi_plugin.vpi -i design.vvp +7777
```

### Xezim

Currently, there is no built-in support for Xezim, but it is possible to use the VPI plugin:

```bash
xezim file1.sv file2.sv ... --vpi-lib ./surfer_vpi_plugin.vpi
```

Again, the port can be changed through a `+` command.

### GHDL - VPI

GHDL supports VPI. However, it is currently not possible to dynamically look up the required symbols from the plugin on Windows (and most likely macos). On Linux, it is possible to compile the plugin under the consideration that `vpi_get_cb_info` currently is not supported by GHDL:

```bash
cargo build --release --package surfer-vpi-plugin --no-default-features --features=ghdl
```

This will use a slightly different version of the code when removing the callbacks that doesn't rely on `vpi_get_cb_info`.

To run it, use:

```bash
ghdl -r my_entity --vpi=./surfer_vpi_plugin.vpi
```

Although it is possible to get the hierarchy, obtaining the simulated values does not fully work.

### Verilator

Still needs to be tested for compatibility.

## VHPI

VHPI (VHDL Procedural Interface) is the analogous to VPI standardized C-language API defined by IEEE 1076 for VHDL simulations. In general, the VHPI support is better on the Surfer side, providing more type information etc, but there are fewer simulators supporting VHPI. In fact, in some cases the VHPI plugin provides more information that the waveform files.

There is a plugin, `surfer_vhpi_plugin`, that can be started with a simulator that supports VHPI. This has been tested and shown to work (to some extent) with NVC. It is included in the downloadable zip-files or can be built locally.

If required, build the plugin:

``` bash
cargo build --release --package surfer-vhpi-plugin
```

Copy the file from `./target/release`, where the filename depends on the platform:

* Linux: `libsurfer_vhpi_plugin.so`
* Macos: `libsurfer_vhpi_plugin.dylib`
* Windows: `surfer_vhpi_plugin.dll`

### NVC

If Surfer can find NVC on your computer, there will be an NVC submenu under Simulation.

#### Start NVC from within Surfer

You can select Simulation -> NVC -> Compile... to compile an arbitrary number of files. These files will be compiled into a `work` directory.

All compiled files in a `work` directory in the current directory will be listed when selecting Simulation -> NVC -> Run simulation... After selecting the correct top-level file, including architecture, it will automatically be elaborated if needed and then the simulation will start.

#### Start NVC separately and connect

Analyze and elaborate as usual:

```bash
nvc -a file1.vhdl file2.vhdl ...
nvc -e top_level
```

Now, when running the simulation provide the VHPI plugin:

```bash
nvc -r top_level --load surfer_vhpi_plugin.dll
```

Which will print something like:

```text
** Note: loading plugin libsurfer_vhpi_plugin.so
** Note: (init): Start of simulation entered
** Note: (init): No valid port startup argument found, using default 6967
** Note: (init): Waiting for RunSimulation message...
```

Now, you can select Simulation -> Connect to running simulator...

It is possible to provide a port number with a `+` argument as

```bash
nvc -r top_level --load surfer_vhpi_plugin.dll +7777
```

### GHDL - VHPI

GHDL also has some basic VHPI support. However, it is not enough for the VHPI plugin. Currently, one get (with VHPI traces on):

```text
loading VHPI library './target/release/libsurfer_vhpi_plugin.so'
vhpi_register_cb ({reason=1034}, 1) return 0000000000000000 [not implemented]
vhpi_check_error (00007FFFD38248F0) return 1
vhpi_register_cb ({reason=1035}, 1) return 0000000000000000 [not implemented]
vhpi_check_error (00007FFFD38248F0) return 1
VHPI module loaded and vhpi_startup_routines functions called!
```

## Running a simulation

Once Surfer has connected to a simulator, the hierarchy is shown. Add variables as usual and then start the simulation.

Starting the simulation can be done using the toolbar, either type a time and press the "play" button or press the "fast forward" button to run the whole simulation. (Pressing "play" without a time will run until the end.)

There is also a restart button, although that currently removes the whole variable setup.

There are also commands for controlling the simulation. See [Interactive simulation](../commands/README.md#interactive-simulation).

## Other simulators

It is also possible to implement the required protocol for communicating between an arbitrary simulator and Surfer. The protocol is defined using protobuf, so it can be used for several languages. See the `surfer-simulator-protocol` crate in the repository.
