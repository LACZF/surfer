`timescale 1ns/1ps

module tb_arrays;
    reg clk;
    reg rst_n;

    // Classic Verilog memory array.
    reg [7:0] mem_u8 [0:15];

    // Unpacked arrays of scalar/integer-like types.
    integer int_arr [0:3];
    // time time_arr [0:1];

    // SystemVerilog packed arrays.
    logic [2:0][7:0] packed_2d;
    logic signed [1:0][15:0] packed_signed_2d;

    // Unpacked arrays with packed elements.
    logic [7:0] unpacked_1d [0:7];
    logic [3:0] unpacked_2d [0:2][0:3];

    // Mixed packed/unpacked multidimensional arrays.
    logic [1:0][7:0] mixed_2d [0:1];
    logic [3:0][1:0][7:0] mixed_3d [0:1][0:2];

    // Net array (driven from regs).
    reg [3:0] net_drv [0:3];
    wire [3:0] net_arr [0:3];

    genvar gi;
    generate
        for (gi = 0; gi < 4; gi = gi + 1) begin : gen_net
            assign net_arr[gi] = net_drv[gi];
        end
    endgenerate

    integer i;
    integer j;

    initial begin
        clk = 1'b0;
        forever #5 clk = ~clk;
    end

    initial begin
        rst_n = 1'b0;

        for (i = 0; i < 16; i = i + 1) begin
            mem_u8[i] = i[7:0];
        end

        for (i = 0; i < 4; i = i + 1) begin
            int_arr[i] = i * 10;
            net_drv[i] = i[3:0];
        end

        // time_arr[0] = 64'd100;
        // time_arr[1] = 64'd250;

        packed_2d = 24'hAD_BEEF;
        packed_signed_2d = 32'sh8001_7FFF;

        for (i = 0; i < 8; i = i + 1) begin
            unpacked_1d[i] = 8'hA0 + i[7:0];
        end

        for (i = 0; i < 3; i = i + 1) begin
            for (j = 0; j < 4; j = j + 1) begin
                unpacked_2d[i][j] = (i + j) & 4'hF;
            end
        end

        mixed_2d[0] = {8'h11, 8'h22};
        mixed_2d[1] = {8'h33, 8'h44};

        for (i = 0; i < 2; i = i + 1) begin
            for (j = 0; j < 3; j = j + 1) begin
                mixed_3d[i][j] = {
                    {8'h10 + i[7:0] + j[7:0], 8'h20 + i[7:0] + j[7:0]},
                    {8'h30 + i[7:0] + j[7:0], 8'h40 + i[7:0] + j[7:0]},
                    {8'h50 + i[7:0] + j[7:0], 8'h60 + i[7:0] + j[7:0]},
                    {8'h70 + i[7:0] + j[7:0], 8'h80 + i[7:0] + j[7:0]}
                };
            end
        end

        #20 rst_n = 1'b1;

        // Keep callback traffic low for stable CI runtime while still creating changes.
        @(posedge clk);
        mem_u8[3] <= 8'h5A;
        int_arr[1] <= 123;
        packed_2d <= packed_2d ^ 24'h00_FF00;
        packed_signed_2d <= packed_signed_2d + 32'sh0000_0001;
        unpacked_1d[2] <= 8'hC3;
        unpacked_2d[1][2] <= 4'hA;
        mixed_2d[0] <= {8'hAA, 8'h55};
        mixed_3d[1][1] <= {
            {8'h01, 8'h02},
            {8'h03, 8'h04},
            {8'h05, 8'h06},
            {8'h07, 8'h08}
        };
        net_drv[2] <= 4'hD;

        #20;
        $finish;
    end
endmodule
