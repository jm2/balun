# frozen_string_literal: true

# Run under `brew ruby`. Select the exact file loader directly: `brew info`
# can resolve a path-loaded formula again through its installed tap/API.
require "formulary"
require "json"
require "pathname"

raise ArgumentError, "expected one installed recipe" unless ARGV.length == 1

recipe = Pathname.new(ARGV.fetch(0)).realpath
formula = Formulary::FromPathLoader.new(recipe).get_formula(:stable)
raise ArgumentError, "recipe loader changed path" unless formula.path.realpath == recipe

puts JSON.generate({ "formulae" => [formula.to_hash], "casks" => [] })
