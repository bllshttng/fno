"""Unit tests for _read_plan_frontmatter helper."""



def test_reads_single_file_plan(tmp_path):
    from fno.graph._intake import _read_plan_frontmatter
    plan = tmp_path / "plan-Y.md"
    plan.write_text("---\nproject: bar\n---\n# title\n")
    fm = _read_plan_frontmatter(str(plan))
    assert fm == {"project": "bar"}


def test_malformed_yaml_returns_empty_with_warning(tmp_path, capsys):
    from fno.graph._intake import _read_plan_frontmatter
    plan = tmp_path / "plan-bad.md"
    plan.write_text("---\n: : :\n---\nbody\n")
    fm = _read_plan_frontmatter(str(plan))
    assert fm == {}
    err = capsys.readouterr().err
    assert "could not parse" in err
    assert str(plan) in err
