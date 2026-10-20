-- Filetype detection for meshfox canvases: `markdown.meshfox`, i.e. still
-- Markdown (its ftplugins/plugins/LSP keep working, and vim.treesitter maps
-- the compound filetype to the `markdown` parser) plus a `meshfox` layer for
-- settings that should only apply to canvases.
--
-- A canvas is either named `*.canvas.md`, or any `*.md` whose first line is
-- the `<!-- meshfox:canvas -->` marker (README.md is one).
local function is_canvas_marker(_, bufnr)
  local first = vim.api.nvim_buf_get_lines(bufnr, 0, 1, false)[1]
  if first and first:match('^%s*<!%-%-%s*meshfox:canvas') then
    return 'markdown.meshfox'
  end
end

vim.filetype.add({
  pattern = {
    ['.*%.canvas%.md'] = { 'markdown.meshfox', { priority = 10 } },
    ['.*%.md'] = { is_canvas_marker, { priority = 10 } },
  },
})

-- Language server: `meshfox lsp` (diagnostics, a node outline, and the
-- "turn this heading into a node" code action -- `gra` on a heading).
-- Needs Neovim 0.11+ (`vim.lsp.config`) and a `meshfox` on $PATH new enough
-- to have the `lsp` subcommand; that is probed once, on the first canvas you
-- open, so an older binary just means no server rather than an error on
-- every file. Point `vim.g.meshfox_lsp_cmd` at another command (a table) to
-- use a different binary (not probed), or set `vim.g.meshfox_lsp_disable =
-- true` to turn the server off.
local function enable_lsp(cmd)
  vim.lsp.config('meshfox', {
    cmd = cmd,
    filetypes = { 'markdown.meshfox' },
    root_markers = { '.meshfox', '.git' },
  })
  vim.lsp.enable('meshfox')
  -- `enable` attaches already-open buffers itself only once startup has
  -- finished; the probe below can return earlier than that, with the
  -- first canvas (`nvim file.canvas.md`) already open.
  vim.cmd.doautoall('nvim.lsp.enable FileType')
end

if vim.lsp and vim.lsp.config and vim.lsp.enable and not vim.g.meshfox_lsp_disable then
  vim.api.nvim_create_autocmd('FileType', {
    pattern = 'markdown.meshfox',
    once = true,
    callback = function()
      if vim.g.meshfox_lsp_cmd then
        return enable_lsp(vim.g.meshfox_lsp_cmd)
      end
      if vim.fn.executable('meshfox') ~= 1 then
        return
      end
      -- `meshfox lsp --help` exits 0 only where the subcommand exists.
      vim.system({ 'meshfox', 'lsp', '--help' }, {}, function(res)
        if res.code == 0 then
          vim.schedule(function() enable_lsp({ 'meshfox', 'lsp' }) end)
        end
      end)
    end,
  })
end

-- `<leader>mn`: turn the heading under the cursor into a meshfox node -- the
-- language server's "Turn ... into a meshfox node" code action applied in one
-- keystroke (the counterpart of `Ctrl-n` in the TUI's source editor), instead
-- of picking it from the `gra` menu. Buffer-local, only in canvases; set
-- `vim.g.meshfox_no_mappings = true` to skip it.
if not vim.g.meshfox_no_mappings then
  vim.api.nvim_create_autocmd('FileType', {
    pattern = 'markdown.meshfox',
    callback = function(ev)
      vim.keymap.set('n', '<leader>mn', function()
        vim.lsp.buf.code_action({
          apply = true,
          filter = function(action)
            -- The single-heading action, not the "all plain headings" one.
            return action.title:match('^Turn ".*" into a meshfox node$') ~= nil
          end,
        })
      end, { buffer = ev.buf, desc = '[M]eshfox: heading to [N]ode' })
    end,
  })
end

-- Color span attribute tails are plain text in tree-sitter-markdown's inline
-- grammar. Highlight their narrow syntax with extmarks, retaining Markdown's
-- existing tree (including bold, links, and code) rather than replacing it.
local color_ns = vim.api.nvim_create_namespace('meshfox.color-spans')
local function highlight_color_spans(buf)
  if not vim.api.nvim_buf_is_valid(buf) or not vim.bo[buf].filetype:match('^markdown') then return end
  vim.api.nvim_buf_clear_namespace(buf, color_ns, 0, -1)
  local function mark(row, first, last, group)
    vim.api.nvim_buf_set_extmark(buf, color_ns, row, first, {
      end_row = row, end_col = last, hl_group = group, priority = 125,
    })
  end
  for row, line in ipairs(vim.api.nvim_buf_get_lines(buf, 0, -1, false)) do
    local cursor = 1
    while true do
      local first, last, attrs = line:find('%]%{([^}\r\n]*)%}', cursor)
      if not first then break end
      cursor = last + 1
      local ok, node = pcall(vim.treesitter.get_node, { bufnr = buf, pos = { row - 1, first - 1 }, ignore_injections = false })
      local code = false
      while ok and node do
        if node:type() == 'code_span' or node:type() == 'fenced_code_block' or node:type() == 'indented_code_block' then code = true; break end
        node = node:parent()
      end
      local seen, captures, pos = {}, {}, 1
      local valid = not code
      while pos <= #attrs do
        local _, whitespace_end = attrs:find('^%s*', pos)
        pos = (whitespace_end or pos - 1) + 1
        if pos > #attrs then break end
        local a, b, key, quote, value = attrs:find('^([%w-]+)=(["\']?)(#%x+)%2', pos)
        if not a or (key ~= 'color' and key ~= 'background-color') or seen[key]
            or (#value ~= 4 and #value ~= 7) or (b < #attrs and not attrs:sub(b + 1, b + 1):match('%s')) then
          valid = false; break
        end
        seen[key] = true
        table.insert(captures, { a, b, key })
        pos = b + 1
      end
      if valid and #captures > 0 then
        -- attrs starts at the zero-based column first + 1, after `]{`.
        mark(row - 1, first, first + 1, '@punctuation.bracket')
        mark(row - 1, last - 1, last, '@punctuation.bracket')
        for _, c in ipairs(captures) do
          local start = first + c[1]
          mark(row - 1, start, start + #c[3], '@property')
          mark(row - 1, start + #c[3], start + #c[3] + 1, '@operator')
          mark(row - 1, start + #c[3] + 1, first + c[2] + 1, '@string')
        end
      end
    end
  end
end
vim.api.nvim_create_autocmd({ 'FileType', 'BufEnter', 'TextChanged', 'TextChangedI' }, {
  callback = function(ev) highlight_color_spans(ev.buf) end,
})
