import re


def slugify(text, max_length=None):
    """Lowercase, replace runs of non-alphanumerics with a hyphen, trim hyphens."""
    slug = re.sub(r"[^a-z0-9]+", "-", text)
    slug = slug.strip("-")
    if max_length:
        slug = slug[:max_length]
    return slug
