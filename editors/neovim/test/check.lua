-- Headless check: every canvas/doc given as an argument must parse with no
-- ERROR/MISSING node in any injected meshfox region, and the filetype rule
-- must classify files the way the README documents.
--   nvim --headless -u NONE -l test/check.lua <files...>
local root = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h')
vim.cmd('filetype plugin on')
vim.opt.runtimepath:append(root)
vim.cmd('runtime plugin/meshfox.lua')

local failed = false
for _, f in ipairs(_G.arg) do
  vim.cmd('silent edit ' .. vim.fn.fnameescape(f))
  local first = vim.api.nvim_buf_get_lines(0, 0, 1, false)[1] or ''
  local want = (f:match('%.canvas%.md$') or first:match('^%s*<!%-%-%s*meshfox:canvas')) and 'markdown.meshfox' or 'markdown'
  if vim.bo.filetype ~= want then
    failed = true
    io.write(('FAIL %s: filetype %s, want %s\n'):format(f, vim.bo.filetype, want))
  end

  local parser = vim.treesitter.get_parser(0)
  parser:parse(true)
  parser:for_each_tree(function(tree, lt)
    if lt:lang() ~= 'meshfox' then return end
    local function walk(n)
      if n:type() == 'ERROR' or n:missing() then
        failed = true
        local r = n:range()
        io.write(('FAIL %s:%d: parse error in meshfox region\n'):format(f, r + 1))
      end
      for c in n:iter_children() do walk(c) end
    end
    walk(tree:root())
  end)
end
io.write(failed and 'FAILED\n' or ('ok (%d files)\n'):format(#_G.arg))
vim.cmd(failed and 'cquit 1' or 'qa!')
