# %%
from pathlib import Path
import json

json_file = Path(__file__).parent / "ggroups.json"
with open(json_file, "r") as file:
    g_groups = json.load(file)
# %%


def create_pest_grammar(g_groups):
    grammar_parts = []

    # Header for the Pest file
    grammar_parts.append(
        "// This Pest grammar file is auto-generated from G-Code definitions."
    )
    grammar_parts.append(
        "// Due to the way pest works, the sorting of the literals is important."
    )
    ggroup_names = []
    for group in g_groups:
        group_rules = []
        for entry in group["entries"]:
            group_rules.append(f'^"{entry["id"]}"')
        ggroup_name = group["short_name"]
        ggroup_names.append(ggroup_name)
        grammar_parts.append(
            f"{ggroup_name} = @{{({' | '.join(reversed(sorted(group_rules)))}) ~ !(ASCII_ALPHANUMERIC) }}"
        )
    grammar_parts.append(f"g_command = {{{' | '.join(ggroup_names)}}}")
    return "\n".join(grammar_parts)


def write_pest_file(grammar, filename=Path(__file__).parent / "ggroups.pest"):
    with open(filename, "w") as file:
        file.write(grammar)


if __name__ == "__main__":
    pest_grammar = create_pest_grammar(g_groups)
    write_pest_file(pest_grammar)
    print("Pest grammar file 'gcode.pest' has been generated.")

# %%
