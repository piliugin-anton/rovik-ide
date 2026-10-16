library ieee;
use ieee.std_logic_1164.all;
entity counter is
  port (clk : in std_logic);
end entity counter;
architecture rtl of counter is
  signal count : integer;
begin
  process (clk) begin end process;
end architecture rtl;
