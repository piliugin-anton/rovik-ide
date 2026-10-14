module counter(input clk, output reg [3:0] q);
  always @(posedge clk) q <= q + 1;
  function [3:0] inc(input [3:0] x); inc = x + 1; endfunction
  task reset; q = 0; endtask
endmodule
