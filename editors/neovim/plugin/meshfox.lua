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
