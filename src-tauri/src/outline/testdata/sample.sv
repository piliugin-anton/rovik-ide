package pkg;
  typedef int myint;
endpackage
module top(input logic clk);
endmodule
class Packet;
  function new(); endfunction
  task send(); endtask
endclass
interface bus_if; logic a; endinterface
