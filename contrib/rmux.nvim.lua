-- Buffer-scoped rmux application control for Neovim running in an rmux buffer.
--
-- Protocol: OSC 777;rmux-app;acquire|release;<pid> terminated by ST. The
-- sequence is written to this process's controlling terminal, so rmux
-- associates it with the buffer running Neovim. It changes only that buffer's
-- application-control state; other rmux shortcuts and buffers are unaffected.
-- Each nested process owns its own PID request. rmux must prune dead
-- PID/start-time owners to recover when VimLeavePre cannot run (for example,
-- after a crash or SIGKILL).

local M = {}
local state = vim.g.rmux_app_control_state or { active = false }
vim.g.rmux_app_control_state = state

local function send(action)
  local ok, tty = pcall(io.open, "/dev/tty", "wb")
  if not ok or not tty then
    return false
  end

  local sequence = string.format("\27]777;rmux-app;%s;%d\27\\", action, vim.fn.getpid())
  local wrote = pcall(function()
    assert(tty:write(sequence))
    assert(tty:flush())
  end)
  pcall(function() tty:close() end)
  return wrote
end

local function acquire()
  if not state.active and send("acquire") then
    state.active = true
  end
end

local function release()
  if state.active and send("release") then
    state.active = false
  end
end

function M.setup()
  local group = vim.api.nvim_create_augroup("RmuxAppControl", { clear = true })
  vim.api.nvim_create_autocmd("VimEnter", { group = group, callback = acquire })
  vim.api.nvim_create_autocmd("VimSuspend", { group = group, callback = release })
  vim.api.nvim_create_autocmd("VimResume", { group = group, callback = acquire })
  vim.api.nvim_create_autocmd("VimLeavePre", { group = group, callback = release })

  -- This file can be sourced after VimEnter, when that event has already run.
  if vim.v.vim_did_enter == 1 then
    acquire()
  end
end

M.setup()
return M
