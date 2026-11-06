CARGO ?= cargo
OUT   := target/dist
APP   := $(OUT)/ErisAutoCaptions.app

export MACOSX_DEPLOYMENT_TARGET := 26.0

.PHONY: app install test clean

# Signed ad hoc unless ERISAUTOCAPTIONS_SIGN names a signing identity: the audio permissions belong
# to the signature, so an ad hoc build has to be allowed again after each rebuild, and a lasting
# identity keeps them.
app:
	$(CARGO) build --release
	rm -rf $(APP)
	mkdir -p $(APP)/Contents/MacOS
	cp resources/Info.plist $(APP)/Contents/
	cp target/release/erisautocaptions $(APP)/Contents/MacOS/ErisAutoCaptions
	codesign --force --sign "$${ERISAUTOCAPTIONS_SIGN:--}" $(APP)

install: app
	mkdir -p ~/Applications
	rm -rf ~/Applications/ErisAutoCaptions.app
	cp -R $(APP) ~/Applications/

test:
	$(CARGO) test

clean:
	$(CARGO) clean
