SUBCOMMANDS = {"team": "fno-agents-mail-team", "send": "fno-agents-mail-send"}


def relay(line, body):
    """Relay one mail send to the native writer as one input line.

    line looks like: send --to <handle> --subject <text>
    body is the message body, which may contain spaces.
    """
    command = SUBCOMMANDS[line.split(" ")[0]]
    return f"{command} {line} --body {body}"
