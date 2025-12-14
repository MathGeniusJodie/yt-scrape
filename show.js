// envionment variables
const channelIdsFile = require.main.path + "/youtube-subs.txt";
const jsonFeedCacheFile = require.main.path + "/youtube-feeds.json";
const maxConcurentRequests = 200;

// utils
const last = (array) => array[array.length - 1];
// array.sort(by("property"))
const by = (by) => (a, b) => 2 * (a[by] > b[by]) - 1;
const { readFile, writeFile } = require("fs").promises;
const exec = require("util").promisify(require("child_process").exec);

const throttle = (originalFunction, maxConcurent) => {
	const queue = [];
	let threads = 0;

	// must use function notation because needs own "this"
	return function (...arguments) {
		return new Promise((resolve) => {
			queue.push({ arguments, resolve, this: this });
			while (threads < maxConcurent && queue.length) {
				threads += 1;
				let task = queue.shift();
				originalFunction
					.apply(task.this, task.arguments)
					.then(task.resolve);
				threads -= 1;
			}
		});
	};
};

const { createGunzip } = require("zlib");
const SaxParser = require("ltx/lib/parsers/ltx");

const atomStreamToJson = (atomStream) => {
	const json = [];
	const inside = {};
	const parserStream = atomStream.pipe(new SaxParser());
	parserStream.on("startElement", (name, attrs) => {
		inside[name] = true;
		if (!inside.entry) return;

		if (name == "entry") json.push({});

		if (inside.link) last(json).link = attrs.href;
		if (inside["media:thumbnail"]) last(json).thumbnailUrl = attrs.url;
	});
	parserStream.on("text", (text) => {
		if (!inside.entry) return;

		if (inside.title) last(json).title = text;
		if (inside.published) last(json).published = text;
		if (inside.name) last(json).name = text;
	});
	return new Promise((resolve) => {
		parserStream.on("endElement", (name) => {
			inside[name] = false;
			if (name == "feed") resolve(json);
		});
	});
};

const channelIdToJsonFeed = throttle((channelId) => {
	return new Promise((resolve) => {
		require("https")
			.get(
				`https://www.youtube.com/feeds/videos.xml?channel_id=${channelId}`,
				{
					headers: { "accept-encoding": "gzip" },
				},
				async (response) => {
					const atomStream = response.pipe(createGunzip());
					const jsonPromise = atomStreamToJson(atomStream);
					const feed = await jsonPromise;

					resolve(feed);
					console.log("downloaded " + channelId);
				}
			)
			.on("error", (e) => {
				console.log("retrying");
				resolve(channelIdToJsonFeed(channelId));
			});
	});
}, maxConcurentRequests);

let cachedJsonFeed = false;
const thumnailWidth = 40;

const scrape = async () => {
	const channelIds = (await readFile(channelIdsFile, "utf8")).split("\n").filter(a=>a);
	const promises = channelIds.map(channelIdToJsonFeed);

	const feeds = await Promise.all(promises);

	let feed = feeds.flat().sort(by("published")).slice(-100);
	console.time("loading thumbnails");
	await Promise.all(
		feed.map(async (entry, index) => {
			const { stdout } = await exec(
				`${require.main.path}/dlimage.sh ${entry.thumbnailUrl} ${thumnailWidth}`
			);
			feed[index].thumbnail = stdout;
		})
	);
	console.timeEnd("loading thumbnails");

	cachedJsonFeed = feed;
	// cache feed in file
	writeFile(jsonFeedCacheFile, JSON.stringify(feed));
};

const getCachedJsonFeed = async () => {
	// if no feed cached in variable, get from file
	return (
		cachedJsonFeed || JSON.parse(await readFile(jsonFeedCacheFile, "utf8"))
	);
};

const lineWrap = (s) =>
	s.split(/(?![^\n]{1,40}$)([^\n]{1,40})\s/).filter((a) => a);

const show = async () => {
	(await getCachedJsonFeed()).map(
		({ title, name, link, thumbnail }, index, { length }) => {
			const number = length - index;
			const titleLines = lineWrap(title);
			const titleLine0 = titleLines[0];
			const titleLine1 = titleLines[1];
			console.log(thumbnail);
			console.log("\x1b[11A");
			console.log(`\x1b[${thumnailWidth+1}C\x1b[1;94m[${number}]`);
			console.log(`\x1b[${thumnailWidth+1}C\x1b[0;97m${titleLine0}`);
			if (titleLine1) console.log(`\x1b[${thumnailWidth+1}C${titleLine1}`);
			console.log(`\x1b[${thumnailWidth+1}C\x1b[2m${name}\x1b[0m`);
			console.log(`\x1b[${thumnailWidth+1}C\x1b[4;34m${link}\x1b[0m`);
			if (!titleLine1) console.log("");
			console.log(`\x1b[${thumnailWidth+1}C\n`);
			console.log(`\x1b[${thumnailWidth+1}C\n`);
		}
	);
};

const play = async (number) => {
	const feed = await getCachedJsonFeed();
	let video = feed[feed.length - number];
	const { spawn } = require('child_process');
	// Spawn mpv detached so it keeps running after Node.js exits
	const child = spawn('mpv', [`--ytdl-format=bestvideo[height<=720]`,video.link], {
		detached: true,
		stdio: 'ignore'
	});
	child.unref();
};

const help = () => {
	console.log(`
\x1b[1mshow: \x1b[0mdisplay results
\x1b[1mscrape: \x1b[0msyncronize results with the internet
\x1b[1m*number*: \x1b[0mplay specific video`);
};

process.stdin.on("data", async (userInput) => {
	if (userInput.includes("show")) {
		show();
	}
	if (userInput.includes("scrape")) {
		scrape();
	}
	if (Number(userInput)) {
		play(userInput);
	}
	if (userInput.includes("help")) {
		help();
	}
});

(async () => {
	if ((process.argv[2] || "").includes("scrape")) {
		await scrape();
	}

	show();
})();

//echo -e "\e[?1000;1006;1015h" # Enable tracking
//echo -e "\e[?1000;1006;1015l" # Disable tracking
// ;34;7M
/*
while true; do
read -rsn1 input
echo "$input"
if [ "$input" = "2" ]; then
		echo "hello world"
		break
fi
done*/
