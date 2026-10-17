"""A module that lives next to python-venv.canvas.md.

The canvas imports it as a plain `import venv_demo_helper`: the `$PYTHON`
launcher puts the block's working directory on PYTHONPATH, so nothing in the
block has to touch `sys.path`. It also uses `tabulate`, which exists only in
the canvas's own virtualenv — a module beside the canvas and a package from
the venv, both importable from the same block.
"""
from tabulate import tabulate


def render(rows):
    return tabulate(rows, headers=["module", "found via"], tablefmt="github")
