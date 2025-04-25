{ writeShellApplication }:

writeShellApplication {
  name = "dev-server";

  runtimeInputs = [ ];

  text = ''
    echo "Hello, World!"
  '';
}
