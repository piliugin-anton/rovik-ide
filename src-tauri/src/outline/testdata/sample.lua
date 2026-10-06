local M = {}
local helper = 1
function M.add(a, b) return a + b end
function M:method() end
local function private() end
M.handler = function(x) return x end
function global_fn() end
return M
