-- nvim --headless -u NONE -i NONE -l editors/neovim/test/colors.lua
local root = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h')
vim.g.meshfox_lsp_disable = true
vim.opt.runtimepath:prepend(root)
dofile(root .. '/plugin/meshfox.lua')
vim.bo.filetype = 'markdown'
vim.api.nvim_buf_set_lines(0, 0, -1, false, {
  '[**Text**]{color="#e06c75" background-color="#fff3b0"}',
  '`[Code]{color="#fff"}`',
  '[Bad]{color="red"}',
  '[Short]{color=#abc}',
})
vim.treesitter.get_parser(0, 'markdown'):parse(true)
vim.api.nvim_exec_autocmds('TextChanged', { buffer = 0 })
local ns = vim.api.nvim_get_namespaces()['meshfox.color-spans']
local marks = vim.api.nvim_buf_get_extmarks(0, ns, 0, -1, { details = true })
local properties = {}
for _, m in ipairs(marks) do
  assert(m[2] ~= 1 and m[2] ~= 2, 'code and invalid colors must remain untouched')
  if m[4].hl_group == '@property' then
    local line = vim.api.nvim_buf_get_lines(0, m[2], m[2]+1, false)[1]
    table.insert(properties, line:sub(m[3]+1, m[4].end_col))
  end
end
assert(table.concat(properties, ',') == 'color,background-color,color', vim.inspect(marks))
print('ok: Neovim color span attributes, code exclusion and short hex')
vim.cmd('qa!')
