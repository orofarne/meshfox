# Required by examples/tangle.canvas.md via `require_relative`.
module TangleHelper
  def self.slugify(text)
    text.downcase.gsub(/[^a-z0-9]+/, "-").gsub(/\A-|-\z/, "")
  end
end
