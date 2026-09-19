# frozen_string_literal: true

# Run under `brew ruby`. Select the exact file loader directly: `brew info`
# can resolve a path-loaded formula again through its installed tap/API.
require "formulary"
require "json"
require "pathname"
require "digest"

MAX_RESOURCES = 256
MAX_PATCHES = 1024
MAX_PATCH_BYTES = 1024 * 1024

# Read declarations only. Never fetch, stage, apply, or inspect a cached download.
def download_record(resource)
  {
    "url" => resource.url.to_s,
    "checksum" => resource.checksum&.hexdigest,
    "revision" => resource.specs[:revision],
    "git" => !!(resource.download_strategy <= GitDownloadStrategy),
  }
end

# Older Homebrew patch objects cannot declare a subdirectory. Newer ones
# expose it; preserve that value whenever the installed API supports it.
def patch_directory(patch)
  patch.respond_to?(:directory) ? patch.directory&.to_s : nil
end

def patch_record(patch, recipe)
  result = { "strip" => patch.strip.to_s }
  case patch
  when ExternalPatch
    resource = patch.resource
    raise ArgumentError, "nested patch declarations are unsupported" unless resource.patches.empty?

    result.merge(
      "kind" => "external", "source" => download_record(resource),
      "directory" => patch_directory(resource), "files" => resource.patch_files.map(&:to_s)
    )
  when DATAPatch, StringPatch
    # A DATA patch normally gets its recipe path immediately before staging.
    # Set it on a copy; do not mutate or stage the evaluated formula.
    selected = patch.dup
    selected.path = recipe if selected.is_a?(DATAPatch)
    contents = selected.contents
    raise ArgumentError, "embedded patch exceeds budget" unless (1..MAX_PATCH_BYTES).cover?(contents.bytesize)

    result.merge(
      "kind" => patch.is_a?(DATAPatch) ? "data" : "string",
      "directory" => patch_directory(patch),
      "size" => contents.bytesize, "sha256" => Digest::SHA256.hexdigest(contents)
    )
  else
    # LocalPatch reads a separate tap/cache file that the installed-recipe
    # checksum does not bind. Do not silently omit it or open that path.
    raise ArgumentError, "unsupported installed patch declaration"
  end
end

def source_inputs(formula, recipe)
  stable = formula.stable
  raise ArgumentError, "missing stable specification" unless stable
  raise ArgumentError, "resource count exceeds budget" if stable.resources.length > MAX_RESOURCES

  patch_count = stable.patches.length + stable.resources.values.sum { |resource| resource.patches.length }
  raise ArgumentError, "patch count exceeds budget" if patch_count > MAX_PATCHES

  resources = stable.resources.map do |name, resource|
    raise ArgumentError, "resource name differs from its key" unless resource.name == name

    download_record(resource).merge(
      "name" => name,
      # Homebrew can infer or inherit this value; it is not a verified version
      # of every component compiled from the resource.
      "version_hint" => resource.version&.to_s,
      "patches" => resource.patches.map { |patch| patch_record(patch, recipe) }
    )
  end
  { "schema" => 1, "resources" => resources,
    "patches" => stable.patches.map { |patch| patch_record(patch, recipe) } }
end

raise ArgumentError, "expected one installed recipe" unless ARGV.length == 1

recipe = Pathname.new(ARGV.fetch(0)).realpath
formula = Formulary::FromPathLoader.new(recipe).get_formula(:stable)
raise ArgumentError, "recipe loader changed path" unless formula.path.realpath == recipe

metadata = formula.to_hash.merge("balun_source_inputs" => source_inputs(formula, recipe))
puts JSON.generate({ "formulae" => [metadata], "casks" => [] })
